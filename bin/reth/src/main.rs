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
use reth::{cli::Cli, FirehoseExecutorBuilder};
use reth_ethereum_cli::chainspec::EthereumChainSpecParser;
use reth_node_ethereum::{node::EthereumAddOns, EthereumNode};
use tracing::info;

fn main() {
    reth_cli_util::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly provided.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
    }

    reth_firehose::init_tracer(firehose_tracer::Tracer::new(firehose_tracer::config::Config {
        chain_client: firehose_tracer::config::ChainClient::Reth,
        ..Default::default()
    }));

    if let Err(err) = Cli::<EthereumChainSpecParser>::parse().run(async move |builder, _| {
        info!(target: "reth::cli", "Launching node");
        let handle = builder
            .with_types::<EthereumNode>()
            .with_components(
                EthereumNode::components().executor(FirehoseExecutorBuilder::default()),
            )
            .with_add_ons(EthereumAddOns::default())
            .install_exex("firehose", |ctx| async move {
                Ok(async move { reth_firehose::run_exex(ctx).await })
            })
            .launch_with_debug_capabilities()
            .await?;

        handle.wait_for_node_exit().await
    }) {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
