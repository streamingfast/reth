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

    if let Err(err) =
        Cli::<EthereumChainSpecParser, FirehoseArgs>::parse().run(async move |builder, args| {
            info!(target: "reth::cli", "Launching node");

            // Resolve the node data directory for the Firehose cursor file default path.
            let data_dir = builder.config().datadir().data_dir().to_path_buf();

            // Build the tracer config from CLI args and init the global tracer.
            let cfg = args.to_tracer_config(&data_dir);
            let shutdown_handle = reth_firehose::init_tracer(cfg);

            let handle = builder
                .node(EthereumNode::default())
                .on_component_initialized(move |node| {
                    // Wire the background writer's drain into the node shutdown lifecycle.
                    if let Some(handle) = shutdown_handle {
                        node.task_executor.spawn_with_graceful_shutdown_signal(
                            |shutdown| async move {
                                let _guard = shutdown.await;
                                handle.drain();
                            },
                        );
                    }
                    Ok(())
                })
                .launch_with_debug_capabilities()
                .await?;

            handle.wait_for_node_exit().await
        })
    {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
