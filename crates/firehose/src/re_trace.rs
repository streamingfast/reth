//! Gap detection and re-trace logic for the Firehose integration.
//!
//! On startup the node may have executed blocks that were never written to stdout
//! (e.g. due to a crash between execution and emission, or because emission was
//! disabled for a period).  This module detects such gaps and re-emits the missing
//! blocks before the sync pipeline resumes.

use std::{ops::RangeInclusive, path::Path};

use eyre::Context as _;
use reth_stages_types::StageId;
use reth_storage_api::{BlockNumReader, StageCheckpointReader};
use tracing::{info, warn};

use crate::GLOBAL_TRACER;

/// Checks for a gap between the cursor file and the execution stage checkpoint,
/// then re-emits any missing blocks.
///
/// Should be called **before** the sync pipeline starts so that consumers of the
/// Firehose output see a contiguous stream of blocks.
///
/// # Errors
///
/// Returns an error if:
/// - The execution stage checkpoint cannot be read.
/// - Historical state required for re-tracing has been pruned (archive node required when a gap is
///   detected).
/// - The re-trace itself fails.
pub fn check_gap_and_re_trace<P>(provider: &P, cursor_path: Option<&Path>) -> eyre::Result<()>
where
    P: StageCheckpointReader + BlockNumReader,
{
    // Read the last block that was successfully written to stdout.
    let last_written = cursor_path.and_then(read_cursor);

    let Some(last_written) = last_written else {
        // No cursor file → either first run or cursor tracking is disabled.
        info!(target: "reth::firehose", "No Firehose cursor found; skipping gap check");
        return Ok(());
    };

    // Read the execution stage tip.
    let exec_checkpoint = provider
        .get_stage_checkpoint(StageId::Execution)
        .context("Failed to read execution stage checkpoint")?;

    let exec_tip = match exec_checkpoint {
        Some(cp) => cp.block_number,
        None => {
            info!(target: "reth::firehose", "Execution stage not yet started; skipping gap check");
            return Ok(());
        }
    };

    if last_written >= exec_tip {
        info!(
            target: "reth::firehose",
            last_written,
            exec_tip,
            "Firehose cursor is current; no gap detected"
        );
        return Ok(());
    }

    let gap = (last_written + 1)..=exec_tip;
    warn!(
        target: "reth::firehose",
        last_written,
        exec_tip,
        gap_size = exec_tip - last_written,
        "Firehose gap detected; re-emitting missing blocks"
    );

    firehose_re_trace_range(gap, provider)
}

/// Re-emits blocks in `range` to the Firehose stdout stream.
///
/// For each block the function:
/// 1. Executes the block through a Firehose-aware executor.
/// 2. Emits the `FIRE BLOCK` line and updates the cursor file.
///
/// # Pruning guard
///
/// If the node has been configured to prune state and the required historical
/// blocks are no longer available, this function returns a fatal error.  Firehose
/// requires an archive (or sufficiently un-pruned) node to be able to re-trace
/// historical blocks.  The pruning check is performed during block execution: when
/// `history_by_block_number` returns an error the function logs a clear fatal message
/// and propagates the error rather than silently skipping blocks.
///
/// # Errors
///
/// Returns an error if historical state is unavailable or re-execution fails.
pub fn firehose_re_trace_range<P>(range: RangeInclusive<u64>, provider: &P) -> eyre::Result<()>
where
    P: StageCheckpointReader + BlockNumReader,
{
    let first = *range.start();

    // ── Re-trace loop ────────────────────────────────────────────────────────
    for block_num in range.clone() {
        re_trace_single_block(block_num, provider).with_context(|| {
            format!(
                "Firehose gap re-trace failed at block {block_num}. \
                 If the error indicates that historical state is unavailable, \
                 the node requires archive-quality state back to block {first}. \
                 Adjust your pruning configuration or restore from a \
                 Firehose-compatible snapshot."
            )
        })?;
    }

    info!(
        target: "reth::firehose",
        start = *range.start(),
        end = *range.end(),
        "Firehose gap re-trace complete"
    );
    Ok(())
}

/// Re-executes a single block and emits its Firehose output.
///
/// # Note on full trace fidelity
///
/// A complete Firehose block includes per-call traces (call stack, storage/balance
/// changes at the opcode level).  Producing that level of detail requires a
/// Firehose-aware EVM inspector wired into the block executor.  The current
/// implementation returns an error directing the caller to integrate the
/// `FirehoseEvmInspector` before re-trace can succeed.
fn re_trace_single_block<P>(block_num: u64, _provider: &P) -> eyre::Result<()>
where
    P: StageCheckpointReader + BlockNumReader,
{
    // Verify the tracer is initialised before attempting any re-execution.
    let _ = GLOBAL_TRACER.get().ok_or_else(|| {
        eyre::eyre!("Firehose tracer not initialized; call init_tracer before re-tracing")
    })?;

    // TODO: Implement full block re-execution with the FirehoseEvmInspector.
    //
    // The complete implementation must:
    //   1. Fetch `RecoveredBlock` via `provider.recovered_block(block_num, ...)`.
    //   2. Obtain a historical state provider via `provider.history_by_block_number(block_num -
    //      1)`.  If this returns a "state pruned" error, propagate it with the message documented
    //      in `firehose_re_trace_range`'s pruning guard note.
    //   3. Create a `State<StateProviderDatabase<_>>` wrapping the state provider.
    //   4. Build a `FirehoseEvmInspector` wrapping the global tracer.
    //   5. Execute the block with `evm_config.executor_for_block(&mut state, &block)`, passing the
    //      inspector so that `on_tx_start/end`, `on_call_enter/exit`, `on_balance_change`,
    //      `on_storage_change`, and `on_log` are called.
    //   6. Call `tracer.on_block_end(None)` to emit the `FIRE BLOCK` line.
    //
    // This requires the EVM config to be passed into this function.  The signature
    // will be extended once `FirehoseEvmInspector` is available.
    eyre::bail!(
        "Full Firehose block re-trace for block {} requires the FirehoseEvmInspector \
         integration which is not yet available in this build. \
         Ensure the node has not missed any blocks before disabling the Firehose tracer.",
        block_num
    )
}

/// Reads the last confirmed block number from the cursor file.
///
/// Returns `None` when the file is absent, cannot be read, or contains content
/// that cannot be parsed as a `u64`.  **Note**: file corruption (unparseable content)
/// is treated the same as a missing file — the gap check is skipped in both cases.
/// Callers that need to distinguish the two cases should read the file directly.
fn read_cursor(path: &Path) -> Option<u64> {
    let content = std::fs::read_to_string(path).ok()?;
    content.trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn read_cursor_returns_none_for_missing_file() {
        assert!(read_cursor(Path::new("/nonexistent/firehose.cursor")).is_none());
    }

    #[test]
    fn read_cursor_parses_valid_file() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "12345").unwrap();
        assert_eq!(read_cursor(f.path()), Some(12345));
    }

    #[test]
    fn read_cursor_returns_none_for_invalid_content() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "not-a-number").unwrap();
        assert!(read_cursor(f.path()).is_none());
    }
}
