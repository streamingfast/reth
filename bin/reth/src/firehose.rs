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
///
/// Registering this with the node builder causes the execution stage to call
/// `execute_and_trace_one` on [`FirehoseBlockExecutor`], which injects
/// [`FirehoseInspector`] and fires all tracer lifecycle hooks.
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
        Ok(FirehoseEvmConfig::new(EthEvmConfig::new(ctx.chain_spec())))
    }
}
