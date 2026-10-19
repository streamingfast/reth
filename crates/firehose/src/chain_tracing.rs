//! Chain-specific tracing configuration and the thread-local active-tracer handle.
//!
//! Some chains route value differently from mainnet Ethereum in ways the generic
//! [`crate::executor::FirehoseWrappedExecutor`] / [`crate::inspector::FirehoseInspector`]
//! cannot infer from the transaction stream alone:
//!
//! * **BSC (Parlia)** credits every transaction's gas fee (and blob fee) to the consensus
//!   `SYSTEM_ADDRESS` (`0xffff…fffe`) instead of the block beneficiary, defers the consensus
//!   "system transactions" found in the block body to the end-of-block finalize step, and sweeps
//!   the accumulated fees from `SYSTEM_ADDRESS` to the validator with direct (non-EVM) state
//!   writes.
//!
//! A chain integrator registers a [`ChainTracingConfig`] once at process startup (next to
//! [`crate::init_tracer`]); the generic executor wrapper and inspector consult it at the
//! relevant decision points. When no config is registered, behavior is mainnet-Ethereum.
//!
//! The second half of this module is the **active-tracer handle**: the block tracer guard
//! ([`crate::FirehoseBlockTracer`]) holds the global tracer mutex for the whole block, so
//! chain executor code that runs *inside* block execution (e.g. BSC's `finish()`-time system
//! transactions) cannot re-lock the global. Instead, the guard registers a thread-local
//! pointer to the tracer for its lifetime, and chain code emits through
//! [`with_active_tracer`].

use alloy_primitives::Address;
use std::{cell::Cell, ptr::NonNull, sync::OnceLock};

/// Chain-specific tracing behavior overrides. See the module docs for background.
#[derive(Debug, Clone, Copy)]
pub struct ChainTracingConfig {
    /// Address credited with transaction fees instead of the block beneficiary
    /// (`None` = beneficiary, the mainnet Ethereum behavior).
    ///
    /// BSC: `SYSTEM_ADDRESS` (`0xffff…fffe`); the fee is swept to the validator later by the
    /// consensus engine.
    pub fee_recipient: Option<Address>,

    /// Emit a `REASON_REWARD_BLOB_FEE` balance change crediting the fee recipient with the
    /// EIP-4844 blob fee (`blob_gas_used × blob_gas_price`). Mainnet Ethereum burns the blob
    /// fee (no balance change); BSC credits it to `SYSTEM_ADDRESS`.
    pub reward_blob_fee: bool,

    /// Returns `true` when a transaction in the block body is a consensus "system transaction"
    /// that the chain's block executor defers: it is skipped during body iteration and executed
    /// by the consensus engine during `finish()`. For such transactions the generic wrapper
    /// emits **nothing** — the chain executor is responsible for emitting the
    /// `on_tx_start`/`on_tx_end` pair (via [`with_active_tracer`]) at actual execution time,
    /// which matches the geth reference where system txs are traced when the consensus engine
    /// applies them.
    ///
    /// Arguments: `to`, the effective gas price at the block's base fee, `signer`,
    /// `block_beneficiary`. For a dynamic-fee transaction the effective price is
    /// `min(max_fee_per_gas, base_fee + max_priority_fee_per_gas)`, so a fee cap above zero with
    /// a zero tip is still a system transaction on BSC.
    pub is_deferred_system_tx: fn(Option<Address>, u128, Address, Address) -> bool,

    /// Wrap the inner executor's `finish()` in an `on_system_call_start`/`end` window.
    ///
    /// `true` (mainnet Ethereum): post-execution work in `finish()` consists of EVM system
    /// calls (EIP-7002/7251 request processing) that belong in `block.system_calls`.
    ///
    /// `false` (BSC): `finish()` runs Parlia finalization — deferred system *transactions* and
    /// direct reward sweeps — which the chain executor emits itself as transaction traces and
    /// block-level balance changes; a system-call window would swallow them.
    pub trace_finish_in_system_call: bool,

    /// Emit a `Some(0)` header base fee as an absent (nil) base fee in the block event.
    ///
    /// BSC (Parlia) has no EIP-1559 base fee, but post-London headers must carry a `0` for
    /// RLP/consensus, so `header.base_fee_per_gas()` is `Some(0)`. The geth Firehose reference
    /// instead emits a nil base fee, which has two observable effects the `0` breaks:
    ///
    /// * the block header omits the base-fee field (matches geth), and
    /// * [`firehose_tracer`]'s effective-gas-price rule reports a dynamic-fee transaction's
    ///   `gas_price` as its fee cap (`max_fee_per_gas`, i.e. geth's `tx.GasPrice()`) instead of
    ///   `min(max_priority_fee + base_fee, max_fee)` — with `base_fee == 0` the latter collapses
    ///   to the tip and diverges from geth.
    ///
    /// `true` (BSC) maps a `Some(0)` header base fee to `None`; a non-zero base fee is preserved.
    /// `false` (mainnet Ethereum) forwards the header value unchanged.
    pub treat_zero_base_fee_as_absent: bool,
}

static CHAIN_TRACING: OnceLock<ChainTracingConfig> = OnceLock::new();

/// Registers the process-wide chain tracing config. Call at most once, at startup, before any
/// block is traced. Panics on double-registration.
pub fn set_chain_tracing_config(config: ChainTracingConfig) {
    CHAIN_TRACING.set(config).expect("set_chain_tracing_config called more than once");
}

/// Returns the registered chain tracing config, if any.
pub fn chain_tracing_config() -> Option<&'static ChainTracingConfig> {
    CHAIN_TRACING.get()
}

thread_local! {
    /// Pointer to the tracer owned by the currently active block guard on this thread, plus a
    /// borrow flag preventing re-entrant use. See [`with_active_tracer`].
    static ACTIVE_TRACER: Cell<Option<NonNull<firehose_tracer::Tracer>>> = const { Cell::new(None) };
    static ACTIVE_TRACER_BORROWED: Cell<bool> = const { Cell::new(false) };
    /// When set, [`crate::inspector::FirehoseInspector`] hooks become no-ops on this thread.
    /// See [`suspend_tracing`].
    static TRACING_SUSPENDED: Cell<bool> = const { Cell::new(false) };
    /// Logs committed by chain-driven (deferred system) transactions in the current block.
    /// See [`add_block_log_offset`].
    static BLOCK_LOG_OFFSET: Cell<u32> = const { Cell::new(0) };
}

/// Adds `n` to the block-wide log offset for the current block.
///
/// The inspector's internal block log counter only advances through the generic wrapper's
/// post-tx accounting, which chain-deferred system transactions bypass (the chain executor
/// emits their traces itself). The chain executor must call this with each system tx's
/// committed log count so subsequent call logs keep block-accurate indices; the offset resets
/// automatically when the next block's tracer guard registers.
pub fn add_block_log_offset(n: u32) {
    BLOCK_LOG_OFFSET.with(|offset| offset.set(offset.get() + n));
}

/// Returns the accumulated block log offset for the current block.
pub(crate) fn block_log_offset() -> u32 {
    BLOCK_LOG_OFFSET.with(|offset| offset.get())
}

/// RAII guard returned by [`suspend_tracing`]. Restores the previous suspension state on drop,
/// so nested suspensions compose.
#[derive(Debug)]
pub struct TracingSuspensionGuard {
    prev: bool,
}

impl Drop for TracingSuspensionGuard {
    fn drop(&mut self) {
        TRACING_SUSPENDED.with(|flag| flag.set(self.prev));
    }
}

/// Suspends Firehose inspector emissions on this thread until the returned guard drops.
///
/// Chain executors use this around *internal* EVM invocations that must not be traced —
/// consensus bookkeeping reads such as BSC's validator-set / turn-length `eth_call`s, which run
/// on the same inspector-carrying EVM as real transactions but are invisible in the geth
/// reference. Without suspension these calls fire inspector hooks while the tracer is between
/// transactions (block state), which is both a parity break and a tracer state-machine panic
/// ("caller expected to be in transaction state").
pub fn suspend_tracing() -> TracingSuspensionGuard {
    let prev = TRACING_SUSPENDED.with(|flag| flag.replace(true));
    TracingSuspensionGuard { prev }
}

/// Returns `true` while a [`TracingSuspensionGuard`] is alive on this thread.
pub(crate) fn is_tracing_suspended() -> bool {
    TRACING_SUSPENDED.with(|flag| flag.get())
}

/// Registers `tracer` as this thread's active tracer. Called by the block tracer guard on
/// construction; must be paired with [`clear_active_tracer`] (the guard's `Drop` does this).
pub(crate) fn set_active_tracer(tracer: &mut firehose_tracer::Tracer) {
    ACTIVE_TRACER.with(|slot| slot.set(Some(NonNull::from(tracer))));
    // New block: reset the per-block system-tx log offset.
    BLOCK_LOG_OFFSET.with(|offset| offset.set(0));
}

/// Clears this thread's active tracer registration.
pub(crate) fn clear_active_tracer() {
    ACTIVE_TRACER.with(|slot| slot.set(None));
}

/// Runs `f` with mutable access to the tracer of the block currently being traced on this
/// thread, or returns `None` when no block guard is active (tracing disabled, or the caller is
/// on a different thread than block execution).
///
/// This is the emission path for chain executor code that runs *inside* block execution and
/// therefore cannot lock the global tracer (the block guard already holds it) — e.g. BSC's
/// end-of-block system transactions and reward sweeps.
///
/// # Safety model
///
/// The pointer aliases the `&mut Tracer` held by the block guard (and borrowed by the EVM
/// inspector during frame callbacks). Soundness rests on temporal exclusivity on a single
/// thread: chain executor code calls this between EVM operations, never from inside an
/// inspector callback. The `ACTIVE_TRACER_BORROWED` flag turns any accidental re-entrant use
/// into a clean `None` instead of aliasing UB.
pub fn with_active_tracer<R>(f: impl FnOnce(&mut firehose_tracer::Tracer) -> R) -> Option<R> {
    let ptr = ACTIVE_TRACER.with(|slot| slot.get())?;
    let already_borrowed = ACTIVE_TRACER_BORROWED.with(|b| b.replace(true));
    if already_borrowed {
        return None;
    }
    // SAFETY: the pointer was registered from a live `&mut Tracer` owned by the block guard on
    // this same thread; the guard's Drop clears the registration before the borrow ends. The
    // borrow flag above rejects re-entrant calls, and inspector callbacks never call this
    // function, so no other `&mut` to the tracer is active during `f`.
    let result = f(unsafe { &mut *ptr.as_ptr() });
    ACTIVE_TRACER_BORROWED.with(|b| b.set(false));
    Some(result)
}
