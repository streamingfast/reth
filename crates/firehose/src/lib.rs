//! Firehose crate providing blockchain data processing modules.
//!
//! This crate contains modules for inspection, mapping, prelude utilities, and running tasks.

/// Block-level drop guard that manages the Firehose tracer lifecycle across validation.
pub mod block_tracer;
/// Executor module with Firehose-aware block executors and EVM configs.
pub mod executor;
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
pub use runner::run_exex;

use std::{
    io::Write,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

static GLOBAL_TRACER: OnceLock<Arc<Mutex<firehose_tracer::Tracer>>> = OnceLock::new();

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
    let lock = init_stdout_lock();
    let writer = SynchronizedStdout::new(lock);
    let tracer = firehose_tracer::Tracer::new_with_writer(config, Box::new(writer));
    GLOBAL_TRACER
        .set(Arc::new(Mutex::new(tracer)))
        .ok()
        .expect("init_tracer called more than once");
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
