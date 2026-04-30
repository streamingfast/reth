#![allow(missing_docs)]

#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

// Required for "override_allocator_on_supported_platforms".
#[cfg(all(feature = "jemalloc", unix))]
use reth_cli_util::allocator::tikv_jemalloc_sys as _;

#[cfg(all(feature = "jemalloc-prof", unix))]
#[unsafe(export_name = "malloc_conf")]
static MALLOC_CONF: &[u8] = b"prof:true,prof_active:true,lg_prof_sample:19\0";

use clap::Parser;
use reth::cli::Cli;
use reth_ethereum_cli::chainspec::EthereumChainSpecParser;
use reth_firehose::FirehoseArgs;
use reth_node_ethereum::EthereumNode;
use tracing::info;

fn main() {
    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
    }

    if let Err(err) = Cli::<EthereumChainSpecParser, FirehoseArgs>::parse().run(
        async move |builder, firehose_args| {
            info!(target: "reth::cli", "Launching node");

            // Resolve the node data directory so the Firehose hook can derive the
            // default cursor file path without needing the full NodeConfig later.
            let data_dir = builder.config().datadir().data_dir().to_path_buf();

            let handle = builder
                .node(EthereumNode::default())
                // ── Firehose startup hook ─────────────────────────────────────────
                // Runs after all node components are built but BEFORE the sync
                // pipeline starts.  This is the correct place to:
                //   1. Initialise the Firehose tracer with the emission config.
                //   2. Detect any gap between the last emitted block (cursor file) and the
                //      execution stage checkpoint.
                //   3. Re-emit the missing blocks before sync resumes.
                //   4. Register the async writer's shutdown handle with the task executor so it is
                //      drained gracefully before process exit.
                .on_component_initialized(move |node| {
                    // Build the tracer config from CLI args and the resolved data dir.
                    let cfg = firehose_args.to_tracer_config(&data_dir);
                    let cursor_path = cfg.cursor_path.clone();

                    // Initialise the global tracer.  Returns a ShutdownHandle when
                    // the emission mode uses a background writer thread.
                    let shutdown_handle = reth_firehose::init_tracer(cfg);

                    // Detect and re-emit any blocks missed since the last run.
                    reth_firehose::check_gap_and_re_trace(&node.provider, cursor_path.as_deref())?;

                    // Wire the background writer's drain into the node shutdown
                    // lifecycle.  The guard held by the async closure keeps the
                    // GracefulShutdown alive until drain() completes, ensuring the
                    // background thread has flushed all buffered blocks before the
                    // process exits.
                    if let Some(handle) = shutdown_handle {
                        node.task_executor.spawn_with_graceful_shutdown_signal(
                            |shutdown| async move {
                                // Wait for the node shutdown signal.
                                let _guard = shutdown.await;
                                // Drain the async writer (blocks until all queued
                                // blocks have been written to stdout).
                                handle.drain();
                            },
                        );
                    }

                    Ok(())
                })
                .launch_with_debug_capabilities()
                .await?;

            handle.wait_for_node_exit().await
        },
    ) {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
