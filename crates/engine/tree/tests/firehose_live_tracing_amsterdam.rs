//! Regression test for Firehose tracing of Amsterdam blocks on the engine-tree live-block path.
//!
//! Amsterdam blocks commit to an EIP-7928 block access list, and post-execution consensus rejects
//! a block whose execution produced no BAL (`BlockAccessListHashMissing`). The Firehose twin of
//! `execute_block` (`execute_and_trace_block`) must therefore build the BAL exactly like upstream
//! does; if it doesn't, every Amsterdam block arriving through `engine_newPayload` is rejected
//! while tracing is on and no `FIRE BLOCK` line is emitted for it.
//!
//! Like `firehose_live_tracing`, it lives in its own integration-test binary because it installs
//! a process-wide tracer.

use eyre::Result;
use reth_chainspec::{EthChainSpec, EthereumHardfork};
use reth_e2e_test_utils::E2ETestSetupExt;
use reth_node_ethereum::EthereumNode;
use reth_provider::BalProvider;

/// Number of blocks to produce, all of them validated through `execute_and_trace_block`.
const PRODUCED_BLOCKS: u64 = 3;

#[tokio::test]
async fn live_payload_validation_traces_amsterdam_blocks() -> Result<()> {
    reth_tracing::init_test_tracing();

    let (mut node, _) =
        EthereumNode::test_setup_for(EthereumHardfork::Amsterdam).build_single().await?;

    // Installed after node startup so the genesis block isn't emitted, only the live blocks.
    let buffer = reth_firehose::init_tracer_with_buffer(
        node.inner.chain_spec().chain().id(),
        Some(0), // shanghai
        Some(0), // cancun
        Some(0), // prague
    );

    let mut hashes = Vec::with_capacity(PRODUCED_BLOCKS as usize);
    for _ in 0..PRODUCED_BLOCKS {
        let payload = node.advance_block().await?;
        hashes.push(payload.block().hash());
    }

    let raw = buffer.get_bytes();
    let text = String::from_utf8(raw).expect("captured tracer output is UTF-8");
    let traced: Vec<u64> = text
        .lines()
        .filter_map(|line| {
            let mut parts = line.split(' ');
            if parts.next()? != "FIRE" || parts.next()? != "BLOCK" {
                return None;
            }
            parts.next()?.parse::<u64>().ok()
        })
        .collect();

    for number in 1..=PRODUCED_BLOCKS {
        assert!(
            traced.contains(&number),
            "expected a FIRE BLOCK line for live Amsterdam block #{number}, got traced blocks \
             {traced:?}"
        );
    }

    // The BAL built by the traced execution is what the node stores for the block.
    for hash in hashes {
        let bal = node.inner.provider.get_bal_by_hash(hash)?;
        assert!(bal.is_some(), "no BAL stored for live Amsterdam block {hash}");
    }

    Ok(())
}
