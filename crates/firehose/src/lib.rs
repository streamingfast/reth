//! Reth ↔ Firehose integration.
//!
//! This crate wires the [`firehose_tracer`] library into the reth node by
//! providing:
//!
//! * **`FirehoseArgs`** — clap argument group that exposes `--firehose.*` CLI flags (emission mode,
//!   channel capacity, live threshold, cursor path).
//! * **`init_tracer`** — one-shot initialisation that stores the tracer in a process-wide
//!   `OnceLock` and returns an optional [`ShutdownHandle`] for the async emission background
//!   thread.
//! * **`check_gap_and_re_trace`** — startup hook that detects gaps between the cursor file and the
//!   execution stage checkpoint, and re-emits the missing blocks before the sync pipeline resumes.
//!
//! # Usage in `bin/reth/src/main.rs`
//!
//! ```rust,ignore
//! use reth_firehose::{FirehoseArgs, init_tracer, check_gap_and_re_trace};
//!
//! // … inside the async run closure …
//! let handle = builder
//!     .node(EthereumNode::default())
//!     .on_component_initialized(move |node| {
//!         let data_dir = node.config.datadir().data_dir().to_path_buf();
//!         let cfg = firehose_args.to_tracer_config(&data_dir);
//!         let cursor_path = cfg.cursor_path.clone();
//!         let shutdown_handle = init_tracer(cfg);
//!
//!         check_gap_and_re_trace(&node.provider, cursor_path.as_deref())?;
//!
//!         if let Some(handle) = shutdown_handle {
//!             node.task_executor.spawn_with_graceful_shutdown_signal(|shutdown| async move {
//!                 let _guard = shutdown.await;
//!                 handle.drain();
//!             });
//!         }
//!         Ok(())
//!     })
//!     .launch_with_debug_capabilities()
//!     .await?;
//! ```

#![doc(
    html_logo_url = "https://raw.githubusercontent.com/paradigmxyz/reth/main/assets/reth-docs.png",
    html_favicon_url = "https://avatars0.githubusercontent.com/u/97369466?s=256",
    issue_tracker_base_url = "https://github.com/paradigmxyz/reth/issues/"
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

pub mod args;
pub mod re_trace;

pub use args::FirehoseArgs;
pub use re_trace::{check_gap_and_re_trace, firehose_re_trace_range};

use firehose_tracer::{config::Config, ShutdownHandle, Tracer};
use std::sync::{Mutex, OnceLock};
use tracing::{debug, info};

/// Process-wide Firehose tracer, initialised at most once per process.
pub static GLOBAL_TRACER: OnceLock<Mutex<Tracer>> = OnceLock::new();

/// Initialise the global Firehose tracer with the given configuration.
///
/// Returns a [`ShutdownHandle`] when the emission mode has an async background
/// thread (i.e. [`EmissionMode::Async`] or [`EmissionMode::Auto`]).  The caller
/// is responsible for calling [`ShutdownHandle::drain`] before the process exits
/// so that the background thread can flush all buffered blocks.
///
/// # Panics
///
/// Panics if called more than once in the same process.  Double-initialisation is
/// treated as a programming error because it would silently discard the first
/// tracer (and any pending shutdown handle), so it is caught eagerly at startup
/// rather than producing silent data loss later.
pub fn init_tracer(config: Config) -> Option<ShutdownHandle> {
    info!(
        target: "reth::firehose",
        cursor_path = ?config.cursor_path,
        "Initialising Firehose tracer"
    );

    let mut tracer = Tracer::new(config);
    let shutdown_handle = tracer.shutdown_handle();

    debug!(
        target: "reth::firehose",
        has_async_thread = shutdown_handle.is_some(),
        "Firehose tracer created"
    );

    GLOBAL_TRACER.set(Mutex::new(tracer)).unwrap_or_else(|_| {
        panic!("init_tracer called more than once; Firehose tracer already initialised");
    });

    shutdown_handle
}

/// Returns a reference to the global [`Mutex<Tracer>`], or `None` if the
/// tracer has not been initialised via [`init_tracer`].
///
/// The lock should be held only for the duration of a single tracer method
/// call to minimise contention.
pub fn get_tracer() -> Option<&'static Mutex<Tracer>> {
    GLOBAL_TRACER.get()
}

#[cfg(test)]
mod tests {
    // NOTE: Because GLOBAL_TRACER is a process-wide OnceLock, unit tests that
    // call init_tracer cannot be run in the same process as each other.
    // The args and re_trace modules have their own self-contained tests.
}
