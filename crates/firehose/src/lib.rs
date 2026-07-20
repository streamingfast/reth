//! Reth ↔ Firehose integration.
//!
//! This crate provides:
//!
//! * **`FirehoseArgs`** — clap argument group that exposes `--firehose.*` CLI flags (emission mode,
//!   channel capacity, live threshold, cursor path).
//! * **`init_tracer`** — one-shot initialisation that stores the tracer in a process-wide
//!   `OnceLock` and returns an optional [`ShutdownHandle`](firehose_tracer::ShutdownHandle) for the
//!   async emission background thread.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod args;
pub use args::FirehoseArgs;

use std::sync::{Mutex, OnceLock};
use tracing::{debug, info};

/// Process-wide Firehose tracer, initialised at most once per process.
static GLOBAL_TRACER: OnceLock<Mutex<firehose_tracer::Tracer>> = OnceLock::new();

/// Initialise the global Firehose tracer with the given configuration.
///
/// Returns a [`ShutdownHandle`](firehose_tracer::ShutdownHandle) when the emission mode has an
/// async background thread. The caller is responsible for calling
/// [`ShutdownHandle::drain`](firehose_tracer::ShutdownHandle::drain) before the process exits so
/// that the background thread can flush all buffered blocks.
///
/// # Panics
///
/// Panics if called more than once in the same process.
pub fn init_tracer(config: firehose_tracer::config::Config) -> Option<firehose_tracer::ShutdownHandle> {
    info!(
        target: "reth::firehose",
        cursor_path = ?config.cursor_path,
        "Initialising Firehose tracer"
    );

    let mut tracer = firehose_tracer::Tracer::new(config);
    let shutdown_handle = tracer.shutdown_handle();

    debug!(
        target: "reth::firehose",
        has_async_thread = shutdown_handle.is_some(),
        "Firehose tracer created"
    );

    GLOBAL_TRACER.set(Mutex::new(tracer)).expect("init_tracer called more than once");

    shutdown_handle
}

/// Returns a reference to the global [`Mutex<Tracer>`], or `None` if the
/// tracer has not been initialised via [`init_tracer`].
pub fn get_tracer() -> Option<&'static Mutex<firehose_tracer::Tracer>> {
    GLOBAL_TRACER.get()
}
