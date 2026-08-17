// ---------------------------------------------------------------------------
// FirehoseExecutorBuilder
// ---------------------------------------------------------------------------

use alloy_evm::eth::spec::EthExecutorSpec;
use reth_ethereum_forks::EthereumHardforks;
use reth_ethereum_primitives::EthPrimitives;
use reth_firehose::{prelude::eyre, FirehoseEvmConfig};
use reth_node_builder::{
    components::ExecutorBuilder,
    node::{FullNodeTypes, NodeTypes},
    BuilderContext,
};
use reth_node_ethereum::EthEvmConfig;

/// Node-builder executor builder that wraps [`EthEvmConfig`] in a [`FirehoseEvmConfig`].
#[derive(Debug, Default, Clone, Copy)]
pub struct FirehoseExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for FirehoseExecutorBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypes<
            ChainSpec: EthExecutorSpec
                           + reth_chainspec::EthChainSpec
                           + EthereumHardforks
                           + reth_ethereum_forks::Hardforks,
            Primitives = EthPrimitives,
        >,
    >,
{
    type EVM = FirehoseEvmConfig<EthEvmConfig<<Node::Types as NodeTypes>::ChainSpec>>;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        // A JIT-compiled frame only calls `log`/`selfdestruct`/`frame_end` on the Inspector —
        // `step`/`step_end` never fire, so per-opcode storage writes and gas-reason data silently
        // vanish from Firehose output while producing no error. This builder never wires JIT
        // support into the resulting `EthEvmConfig` in the first place (unlike
        // `EthereumExecutorBuilder::build_evm`, which honors `--jit` via `build_evm_config`), but
        // refuse to start outright if `--jit` was passed so a misconfigured node fails loudly
        // instead of silently ignoring the flag. The `reth_jit` RPC method is guarded separately
        // (crates/rpc/rpc/src/reth.rs) since JIT can still be toggled at runtime regardless of how
        // the node started.
        if ctx.config().jit.enabled {
            eyre::bail!(
                "refusing to start with --jit while the Firehose tracer is active: JIT-compiled \
                 execution does not fire the Inspector step/step_end hooks Firehose relies on \
                 for per-opcode tracing"
            );
        }

        Ok(FirehoseEvmConfig::new(EthEvmConfig::new(ctx.chain_spec())))
    }
}
