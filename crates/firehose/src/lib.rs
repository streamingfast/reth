//! Firehose crate providing blockchain data processing modules.
//!
//! This crate contains modules for inspection, mapping, prelude utilities, and running tasks.

/// Block-level drop guard that manages the Firehose tracer lifecycle across validation.
pub mod block_tracer;
/// Executor module with Firehose-aware block executors and EVM configs.
pub mod executor;
/// Resolves which finalized block a Firehose block event may advertise.
pub mod finality;
/// Inspector module for analyzing blockchain data.
pub mod inspector;
/// Mapper module for transforming blockchain data.
pub mod mapper;
/// Prelude module with common imports and utilities.
pub mod prelude;
/// Runner module for executing processing tasks.
pub mod runner;

pub use block_tracer::{FirehoseBlockTracer, GlobalTracerGuard};
pub use executor::{
    run_wrapped_block, ChainHooks, FirehoseBlockExecutor, FirehoseEvmConfig,
    FirehoseWrappedExecutor, NoChainHooks, NoPostTxExtras, NoPreTxAdjust, PostTxExtras,
    PreTxAdjust,
};
pub use finality::finalized_ref_for_block;
pub use firehose_tracer::types::FinalizedBlockRef;
pub use runner::{emit_genesis_block_if_empty, emit_genesis_block_on_empty_chain, run_exex};

use std::{
    io::Write,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

static GLOBAL_TRACER: OnceLock<Arc<Mutex<firehose_tracer::Tracer>>> = OnceLock::new();

/// Chain-level fee semantics that the generic Firehose accounting cannot infer from the EVM.
///
/// The `FirehoseWrappedExecutor` derives the post-transaction `RewardTransactionFee` balance
/// change for the block beneficiary from `gas_used × (effective_gas_price − burned_base_fee)`.
/// On Ethereum mainnet the EIP-1559 base fee is burned, so the beneficiary only receives the
/// priority fee. Chains whose handler credits the *whole* effective gas price to the beneficiary
/// (no burn — e.g. Arc's `reward_beneficiary`) must report `base_fee_burned: false`, otherwise
/// every coinbase reward is under-reported by `gas_used × base_fee` and the emitted
/// `new_value` disagrees with the account's on-chain balance.
///
/// Set once per process through [`init_tracer_with_semantics`] (or the buffer-backed variant);
/// [`init_tracer`] keeps the mainnet default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainSemantics {
    /// Whether the EIP-1559 base fee is burned (`true`, Ethereum mainnet) or credited to the
    /// block beneficiary together with the priority fee (`false`).
    pub base_fee_burned: bool,
}

impl ChainSemantics {
    /// Ethereum mainnet semantics: the base fee is burned.
    pub const MAINNET: Self = Self { base_fee_burned: true };
    /// Chains that credit the full effective gas price (base + priority fee) to the beneficiary.
    pub const BASE_FEE_TO_BENEFICIARY: Self = Self { base_fee_burned: false };
}

impl Default for ChainSemantics {
    fn default() -> Self {
        Self::MAINNET
    }
}

static CHAIN_SEMANTICS: OnceLock<ChainSemantics> = OnceLock::new();

/// Returns the process-wide [`ChainSemantics`], falling back to [`ChainSemantics::MAINNET`]
/// when the tracer was initialized without an explicit value (or not at all).
pub fn chain_semantics() -> ChainSemantics {
    CHAIN_SEMANTICS.get().copied().unwrap_or_default()
}

/// Records the process-wide [`ChainSemantics`]. Called by the `init_tracer*` functions; must
/// run at most once per process.
fn set_chain_semantics(semantics: ChainSemantics) {
    CHAIN_SEMANTICS
        .set(semantics)
        .ok()
        .expect("chain semantics already initialized (init_tracer called more than once)");
}

/// The portion of `base_fee` that leaves circulation on this chain, i.e. what the beneficiary
/// does **not** receive. This is the `base_fee` argument the post-tx balance accounting expects.
pub(crate) const fn burned_base_fee(base_fee: u64, semantics: ChainSemantics) -> u64 {
    if semantics.base_fee_burned {
        base_fee
    } else {
        0
    }
}

/// Process-wide stdout write lock.
///
/// When two [`firehose_tracer::Tracer`] instances exist simultaneously (e.g. the global
/// live-block tracer and a flashblock-specific tracer), their writes to stdout must not
/// interleave. Both tracers receive a [`SynchronizedStdout`] backed by this same
/// `Arc<Mutex<()>>` so each `write_all` call is serialised.
///
/// Initialized by [`init_stdout_lock`] and retrieved by [`stdout_lock`].
static STDOUT_LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();

/// Initialize the process-wide stdout write lock, or return the existing one.
///
/// Idempotent: subsequent calls return the same lock. Called automatically by [`init_tracer`];
/// there is no need to call this directly unless constructing a tracer outside of that path.
/// Returns the lock so callers can wrap it in a [`SynchronizedStdout`] for additional tracers
/// (e.g. a flashblock tracer).
pub fn init_stdout_lock() -> Arc<Mutex<()>> {
    STDOUT_LOCK.get_or_init(|| Arc::new(Mutex::new(()))).clone()
}

/// Returns the process-wide stdout write lock.
///
/// Panics if [`init_tracer`] (or [`init_stdout_lock`]) has not been called yet.
pub fn stdout_lock() -> Arc<Mutex<()>> {
    STDOUT_LOCK.get().expect("stdout lock not initialized — call init_tracer first").clone()
}

/// A `Write` implementation that serialises stdout writes across multiple tracer instances.
///
/// Each call to `write` / `write_all` / `flush` acquires the shared `Arc<Mutex<()>>`
/// before delegating to [`std::io::stdout`]. When only one tracer is active the lock is
/// uncontested and the overhead is negligible.
#[derive(Debug)]
pub struct SynchronizedStdout {
    lock: Arc<Mutex<()>>,
}

impl SynchronizedStdout {
    /// Creates a new `SynchronizedStdout` backed by the given lock.
    pub fn new(lock: Arc<Mutex<()>>) -> Self {
        Self { lock }
    }
}

impl Write for SynchronizedStdout {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _guard = self.lock.lock().expect("stdout lock poisoned");
        std::io::stdout().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _guard = self.lock.lock().expect("stdout lock poisoned");
        std::io::stdout().flush()
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        let _guard = self.lock.lock().expect("stdout lock poisoned");
        std::io::stdout().write_all(buf)
    }
}

/// Returns `true` if the process-wide tracer has been initialized via [`init_tracer`].
///
/// Use this for zero-cost checks at call sites that should only run when Firehose is active.
pub fn is_tracer_initialized() -> bool {
    GLOBAL_TRACER.get().is_some()
}

/// Initialize the process-wide tracer and stdout lock in a single call.
///
/// Initialises the shared [`STDOUT_LOCK`], wraps it in a [`SynchronizedStdout`], and constructs
/// the [`firehose_tracer::Tracer`] with that writer. Callers that create additional tracers
/// (e.g. a flashblock tracer) can retrieve the same lock via [`stdout_lock`] and wrap it in
/// their own [`SynchronizedStdout`], ensuring all tracer writes are serialised.
///
/// Must be called exactly once before any call to [`tracer`]. Panics if called more than once.
pub fn init_tracer(config: firehose_tracer::config::Config) {
    init_tracer_with_semantics(config, ChainSemantics::default())
}

/// [`init_tracer`] with explicit [`ChainSemantics`] for chains whose fee distribution differs
/// from Ethereum mainnet (see [`ChainSemantics`]).
///
/// Must be called exactly once before any call to [`tracer`]. Panics if called more than once.
pub fn init_tracer_with_semantics(
    config: firehose_tracer::config::Config,
    semantics: ChainSemantics,
) {
    set_chain_semantics(semantics);
    let lock = init_stdout_lock();
    let writer = SynchronizedStdout::new(lock);
    let tracer = firehose_tracer::Tracer::new_with_writer(config, Box::new(writer));
    GLOBAL_TRACER
        .set(Arc::new(Mutex::new(tracer)))
        .ok()
        .expect("init_tracer called more than once");
}

/// Initialize the process-wide tracer to capture all output into an in-memory buffer, returning a
/// handle to read it back.
///
/// This is the buffer-backed counterpart to [`init_tracer`] (which writes to stdout): it builds a
/// fully blockchain-initialized [`firehose_tracer::Tracer`] over a
/// [`firehose_tracer::InMemoryBuffer`] and installs it as the process-wide tracer, so the live
/// engine path — [`is_tracer_initialized`] plus [`block_tracer::FirehoseBlockTracer::start`] —
/// becomes active and every `FIRE BLOCK` line is captured instead of printed.
///
/// Intended for integration tests that drive the real validation path and need to assert on the
/// emitted Firehose blocks. Like [`init_tracer`], it must be called at most once per process.
///
/// `shanghai_time` / `cancun_time` / `prague_time` are the timestamp-based fork activations the
/// tracer uses when mapping block contents (`Some(0)` = active from genesis, `None` = never). They
/// do not gate whether a block is emitted.
pub fn init_tracer_with_buffer(
    chain_id: u64,
    shanghai_time: Option<u64>,
    cancun_time: Option<u64>,
    prague_time: Option<u64>,
) -> firehose_tracer::InMemoryBuffer {
    init_tracer_with_buffer_and_semantics(
        chain_id,
        shanghai_time,
        cancun_time,
        prague_time,
        ChainSemantics::default(),
    )
}

/// [`init_tracer_with_buffer`] with explicit [`ChainSemantics`], for integration tests of chains
/// whose fee distribution differs from Ethereum mainnet.
pub fn init_tracer_with_buffer_and_semantics(
    chain_id: u64,
    shanghai_time: Option<u64>,
    cancun_time: Option<u64>,
    prague_time: Option<u64>,
    semantics: ChainSemantics,
) -> firehose_tracer::InMemoryBuffer {
    set_chain_semantics(semantics);
    // Mirror `init_tracer`: ensure the shared stdout lock exists so any code path that later
    // reaches for it (e.g. an additional flashblock tracer) does not panic.
    let _ = init_stdout_lock();
    let (tracer, buffer) = firehose_tracer::Tracer::with_buffer(
        firehose_tracer::config::Config::default(),
        firehose_tracer::config::ChainConfig {
            chain_id,
            shanghai_time,
            cancun_time,
            prague_time,
            verkle_time: None,
        },
        "reth-firehose",
        env!("CARGO_PKG_VERSION"),
    );
    GLOBAL_TRACER
        .set(Arc::new(Mutex::new(tracer)))
        .ok()
        .expect("init_tracer/init_tracer_with_buffer called more than once");
    buffer
}

/// Acquire exclusive access to the process-wide tracer.
///
/// Panics if [`init_tracer`] has not been called yet, or if the mutex is poisoned.
pub fn tracer() -> MutexGuard<'static, firehose_tracer::Tracer> {
    GLOBAL_TRACER
        .get()
        .expect("firehose tracer not initialized — call init_tracer first")
        .lock()
        .expect("firehose tracer mutex poisoned")
}

#[cfg(test)]
mod chain_semantics_tests {
    use super::{burned_base_fee, ChainSemantics};

    #[test]
    fn default_is_mainnet_burn() {
        assert_eq!(ChainSemantics::default(), ChainSemantics::MAINNET);
        assert!(ChainSemantics::default().base_fee_burned);
    }

    #[test]
    fn burned_base_fee_follows_semantics() {
        assert_eq!(burned_base_fee(7, ChainSemantics::MAINNET), 7);
        assert_eq!(burned_base_fee(7, ChainSemantics::BASE_FEE_TO_BENEFICIARY), 0);
        assert_eq!(burned_base_fee(0, ChainSemantics::MAINNET), 0);
    }
}
