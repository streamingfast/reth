//! Block-level drop guard that owns the Firehose tracer lifecycle for a single block.
//!
//! A [`FirehoseBlockTracer`] acquires the global tracer lock and emits `on_block_start`
//! (or `on_genesis_block`) on construction. The caller must later consume the guard via
//! [`FirehoseBlockTracer::mark_verified`] (flushes the block to stdout) or
//! [`FirehoseBlockTracer::mark_failed`] (discards it). If the guard is dropped without
//! being consumed, it emits `on_block_end(Some(err))` as a safety net so incomplete
//! blocks are never flushed as valid.
//!
//! This type exists so that the `on_block_end` call can be deferred until **after** all
//! post-execution validation (receipt root, state root, consensus) has completed. Without
//! this deferral, invalid blocks would be flushed to the downstream Firehose consumer
//! before their failure is detected.

use alloy_consensus::BlockHeader;
use alloy_primitives::Sealable;
use reth_node_api::NodePrimitives;
use reth_primitives_traits::{Block as BlockTrait, BlockBody, RecoveredBlock};
use std::sync::MutexGuard;

use crate::{inspector::FirehoseInspector, mapper};

/// Drop guard wrapping the global tracer's `MutexGuard` for the duration of a single block.
///
/// See the module-level documentation for the full lifecycle contract.
pub struct FirehoseBlockTracer {
    guard: MutexGuard<'static, firehose_tracer::Tracer>,
    status: Status,
    /// `true` if this guard was created for block 1 (the genesis marker).
    is_genesis: bool,
}

impl FirehoseBlockTracer {
    /// Acquires the global tracer and emits the start-of-block event.
    ///
    /// For block 1 this emits `on_genesis_block` (with an empty genesis alloc — the caller
    /// is expected not to rely on it for historical sync). For all other blocks it emits
    /// `on_block_start`.
    pub fn start<N>(block: &RecoveredBlock<N::Block>) -> Self
    where
        N: NodePrimitives,
        N::Block: BlockTrait,
        <N::Block as BlockTrait>::Header: BlockHeader + Sealable,
        <N::Block as BlockTrait>::Body: BlockBody,
        <<N::Block as BlockTrait>::Body as BlockBody>::OmmerHeader: BlockHeader + Sealable,
    {
        let mut guard = crate::tracer();
        let is_genesis = block.number() == 1;
        if is_genesis {
            guard.on_genesis_block(
                firehose_tracer::types::BlockEvent {
                    block: mapper::to_block_data_eth::<N>(block),
                    finalized: None,
                    flash_block: None,
                },
                Default::default(),
            );
        } else {
            guard.on_block_start(firehose_tracer::types::BlockEvent {
                block: mapper::to_block_data_eth::<N>(block),
                finalized: None,
                flash_block: None,
            });
        }
        Self { guard, status: Status::Started, is_genesis }
    }

    /// Returns `true` if this guard was created for the genesis marker (block 1).
    pub const fn is_genesis(&self) -> bool {
        self.is_genesis
    }

    /// Returns a mutable reference to the held tracer, for in-block event emission.
    pub fn tracer_mut(&mut self) -> &mut firehose_tracer::Tracer {
        &mut self.guard
    }

    /// Builds a [`FirehoseInspector`] that borrows the tracer held by this guard.
    ///
    /// The returned inspector is valid for as long as `self` is not otherwise borrowed.
    /// Typically this inspector is moved into an EVM via `evm_with_env_and_inspector`,
    /// then dropped automatically when the executor finishes — at which point the tracer
    /// becomes accessible again via [`Self::tracer_mut`] / [`Self::mark_verified`].
    pub fn inspector(&mut self) -> FirehoseInspector<'_> {
        FirehoseInspector::new(&mut self.guard)
    }

    /// Consumes the guard and emits `on_block_end(None)`, flushing the block to stdout.
    ///
    /// Call this only after **all** post-execution validations (receipt root, state root,
    /// consensus checks) have succeeded. Calling it earlier risks flushing a block that
    /// later turns out to be invalid.
    ///
    /// For the genesis marker (block 1), this is a no-op: `on_genesis_block` already
    /// flushed the block during [`Self::start`] and the Firehose protocol does not expect
    /// a matching `on_block_end`.
    pub fn mark_verified(mut self) {
        if !self.is_genesis {
            self.guard.on_block_end(None);
        }
        self.status = Status::Consumed;
    }

    /// Consumes the guard and emits `on_block_end(Some(err))`, discarding the block.
    ///
    /// For the genesis marker (block 1), this is a no-op: the `on_genesis_block` event
    /// has already been flushed and cannot be retracted.
    pub fn mark_failed(mut self, err: &dyn std::error::Error) {
        if !self.is_genesis {
            self.guard.on_block_end(Some(err));
        }
        self.status = Status::Consumed;
    }
}

impl Drop for FirehoseBlockTracer {
    fn drop(&mut self) {
        // Safety net: any early-return path that fails to call mark_verified/mark_failed
        // ends here. Treat it as failure so the block is discarded rather than flushed.
        // Genesis is exempt: `on_genesis_block` was emitted standalone and has no matching
        // end event.
        if matches!(self.status, Status::Started) && !self.is_genesis {
            let err = std::io::Error::other(
                "FirehoseBlockTracer dropped without mark_verified/mark_failed",
            );
            self.guard.on_block_end(Some(&err));
        }
    }
}

#[derive(Debug)]
enum Status {
    /// `on_block_start` has been emitted; awaiting `mark_verified` or `mark_failed`.
    Started,
    /// `on_block_end` has already been emitted; Drop must not emit it again.
    Consumed,
}
