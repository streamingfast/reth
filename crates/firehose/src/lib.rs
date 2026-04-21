//! Firehose crate providing blockchain data processing modules.
//!
//! This crate contains modules for inspection, mapping, prelude utilities, and running tasks.

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

pub use executor::FirehoseEvmConfig;
pub use runner::run_exex;

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

static GLOBAL_TRACER: OnceLock<Arc<Mutex<firehose_tracer::Tracer>>> = OnceLock::new();

/// Initialize the process-wide tracer instance.
///
/// Must be called exactly once before any call to [`tracer`]. Panics if called more than once.
pub fn init_tracer(t: firehose_tracer::Tracer) {
    GLOBAL_TRACER.set(Arc::new(Mutex::new(t))).ok().expect("init_tracer called more than once");
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
