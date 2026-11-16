//! Replays of a block's transactions run through the chain's block executor.

use alloy_consensus::Header;
use alloy_eips::BlockNumberOrTag;
use alloy_network::eip2718::Encodable2718;
use alloy_primitives::{Address, Bytes, TxKind, B256, U256};
use alloy_provider::{ext::DebugApi, Provider, ProviderBuilder};
use alloy_rpc_types_engine::ExecutionData;
use alloy_rpc_types_eth::{TransactionReceipt, TransactionRequest};
use alloy_rpc_types_trace::geth::{
    DiffMode, GethDebugTracingOptions, GethTrace, PreStateConfig, PreStateFrame, TraceResult,
};
use jsonrpsee_core::client::ClientT;
use reth_chainspec::{ChainSpec, EthereumHardfork};
use reth_e2e_test_utils::{transaction::TransactionTestContext, wallet::Wallet, E2ETestSetupExt};
use reth_ethereum_primitives::{Block, EthPrimitives, Receipt, TransactionSigned, TxType};
use reth_evm::{
    block::{
        BlockExecutionError, BlockExecutionResult, BlockExecutor, BlockExecutorFactory,
        ExecutableTx, GasOutput, StateDB,
    },
    eth::{
        EthBlockExecutionCtx, EthBlockExecutor, EthBlockExecutorFactory, EthEvmContext, EthTxResult,
    },
    ConfigureEngineEvm, ConfigureEvm, EvmEnvFor, EvmFactory, ExecutableTxIterator, ExecutionCtxFor,
    NextBlockEnvAttributes, RecoveredTx,
};
use reth_evm_ethereum::{
    factory::RethEvmFactory, EthBlockAssembler, EthEvmConfig, RethReceiptBuilder,
};
use reth_node_builder::{
    components::{BasicPayloadServiceBuilder, ComponentsBuilder, ExecutorBuilder},
    node::{FullNodeTypes, NodeTypes},
    BuilderContext, Node, NodeAdapter,
};
use reth_node_ethereum::{
    EthEngineTypes, EthereumAddOns, EthereumConsensusBuilder, EthereumEngineValidatorBuilder,
    EthereumEthApiBuilder, EthereumNetworkBuilder, EthereumNode, EthereumPayloadBuilder,
    EthereumPoolBuilder,
};
use reth_primitives_traits::{SealedBlock, SealedHeader};
use reth_provider::EthStorage;
use revm::{context::result::HaltReason, Inspector};
use std::sync::Arc;

/// Amount the test executor credits to the sender of every transaction.
const SETTLEMENT: U256 = U256::from_limbs([1_000_000_000, 0, 0, 0]);

/// Traces and intermediate roots of a block's transactions include the changes the block executor
/// applies on top of the EVM result, so they agree with the committed state.
#[tokio::test]
async fn replays_include_block_executor_settlement() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (mut node, wallet) =
        SettlingNode::test_setup_for(EthereumHardfork::Prague).build_single().await?;
    let provider = ProviderBuilder::new().connect_http(node.rpc_url());
    let client = node.rpc_client().expect("the HTTP RPC server is enabled");
    let sender = wallet.inner.address();
    let recipient = Address::with_last_byte(0x77);
    let values = [U256::from(1), U256::from(2)];

    let mut tx_hashes = Vec::new();
    for (nonce, value) in values.into_iter().enumerate() {
        let raw_tx = transfer(&wallet, recipient, value, nonce as u64).await;
        tx_hashes.push(node.rpc.inject_tx(raw_tx).await?);
    }
    let payload = node.advance_block().await?;
    let block_hash = payload.block().hash();
    let block_number = payload.block().number;
    let state_root = payload.block().state_root;
    node.assert_new_block(tx_hashes[0], block_hash, block_number).await?;

    // The executor's settlement is part of the committed state.
    let mut balance = provider.get_balance(sender).number(0).await?;
    let mut expected_diffs = Vec::new();
    for (tx_hash, value) in tx_hashes.into_iter().zip(values) {
        let receipt = provider
            .get_transaction_receipt(tx_hash)
            .await?
            .ok_or_else(|| eyre::eyre!("missing receipt for {tx_hash}"))?;
        let pre = balance;
        balance = pre - value - fee(&receipt) + SETTLEMENT;
        expected_diffs.push((tx_hash, pre, balance));
    }
    assert_eq!(provider.get_balance(sender).number(1).await?, balance);

    let opts = GethDebugTracingOptions::prestate_tracer(PreStateConfig {
        diff_mode: Some(true),
        ..Default::default()
    });
    let mut diffs = Vec::new();
    for (tx_hash, pre, post) in expected_diffs {
        let trace = provider.debug_trace_transaction(tx_hash, opts.clone()).await?;
        let diff = prestate_diff(trace)?;
        assert_eq!(diff.pre[&sender].balance, Some(pre));
        assert_eq!(diff.post[&sender].balance, Some(post));
        diffs.push(diff);
    }

    let block_traces =
        provider.debug_trace_block_by_number(BlockNumberOrTag::Number(1), opts).await?;
    let block_diffs = block_traces
        .into_iter()
        .map(|result| match result {
            TraceResult::Success { result, .. } => prestate_diff(result),
            TraceResult::Error { error, .. } => Err(eyre::eyre!("trace failed: {error}")),
        })
        .collect::<eyre::Result<Vec<_>>>()?;
    assert_eq!(block_diffs, diffs);

    let roots =
        client.request::<Vec<B256>, _>("debug_intermediateRoots", (block_hash, None::<()>)).await?;
    assert_eq!(roots.len(), 2);
    assert_eq!(roots.last(), Some(&state_root));

    Ok(())
}

/// Signs a transfer of `value` from the wallet's account to `recipient`, encoded for
/// `eth_sendRawTransaction`.
async fn transfer(wallet: &Wallet, recipient: Address, value: U256, nonce: u64) -> Bytes {
    let request = TransactionRequest {
        chain_id: Some(wallet.chain_id),
        gas: Some(21_000),
        max_fee_per_gas: Some(20_000_000_000),
        max_priority_fee_per_gas: Some(1_000_000_000),
        nonce: Some(nonce),
        to: Some(TxKind::Call(recipient)),
        value: Some(value),
        ..Default::default()
    };
    TransactionTestContext::sign_tx(wallet.inner.clone(), request).await.encoded_2718().into()
}

fn fee(receipt: &TransactionReceipt) -> U256 {
    U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price)
}

fn prestate_diff(trace: GethTrace) -> eyre::Result<DiffMode> {
    match trace {
        GethTrace::PreStateTracer(PreStateFrame::Diff(diff)) => Ok(diff),
        trace => Err(eyre::eyre!("expected a prestate diff, got {trace:?}")),
    }
}

/// Ethereum node whose block executor credits `SETTLEMENT` to every transaction's sender.
#[derive(Debug, Default, Clone, Copy)]
struct SettlingNode;

impl NodeTypes for SettlingNode {
    type Primitives = EthPrimitives;
    type ChainSpec = ChainSpec;
    type Storage = EthStorage;
    type Payload = EthEngineTypes;
}

impl<N> Node<N> for SettlingNode
where
    N: FullNodeTypes<Types = Self>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        EthereumPoolBuilder,
        BasicPayloadServiceBuilder<EthereumPayloadBuilder>,
        EthereumNetworkBuilder,
        SettlingExecutorBuilder,
        EthereumConsensusBuilder,
    >;

    type AddOns =
        EthereumAddOns<NodeAdapter<N>, EthereumEthApiBuilder, EthereumEngineValidatorBuilder>;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        EthereumNode::components().executor(SettlingExecutorBuilder)
    }

    fn add_ons(&self) -> Self::AddOns {
        EthereumAddOns::default()
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct SettlingExecutorBuilder;

impl<N> ExecutorBuilder<N> for SettlingExecutorBuilder
where
    N: FullNodeTypes<Types = SettlingNode>,
{
    type EVM = SettlingEvmConfig;

    async fn build_evm(self, ctx: &BuilderContext<N>) -> eyre::Result<Self::EVM> {
        let inner = EthEvmConfig::new_with_evm_factory(ctx.chain_spec(), RethEvmFactory::default());
        let executor_factory = SettlingExecutorFactory { inner: inner.executor_factory.clone() };
        Ok(SettlingEvmConfig { inner, executor_factory })
    }
}

#[derive(Debug, Clone)]
struct SettlingEvmConfig {
    inner: EthEvmConfig<ChainSpec, RethEvmFactory>,
    executor_factory: SettlingExecutorFactory,
}

impl ConfigureEvm for SettlingEvmConfig {
    type Primitives = EthPrimitives;
    type Error = <EthEvmConfig<ChainSpec, RethEvmFactory> as ConfigureEvm>::Error;
    type NextBlockEnvCtx = NextBlockEnvAttributes;
    type BlockExecutorFactory = SettlingExecutorFactory;
    type BlockAssembler = EthBlockAssembler<ChainSpec>;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        &self.executor_factory
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        self.inner.block_assembler()
    }

    fn evm_env(&self, header: &Header) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env(header)
    }

    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &NextBlockEnvAttributes,
    ) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.next_evm_env(parent, attributes)
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<Block>,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_block(block)
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<ExecutionCtxFor<'_, Self>, Self::Error> {
        self.inner.context_for_next_block(parent, attributes)
    }
}

impl ConfigureEngineEvm<ExecutionData> for SettlingEvmConfig {
    fn evm_env_for_payload(&self, payload: &ExecutionData) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env_for_payload(payload)
    }

    fn context_for_payload<'a>(
        &self,
        payload: &'a ExecutionData,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_payload(payload)
    }

    fn tx_iterator_for_payload(
        &self,
        payload: &ExecutionData,
    ) -> Result<impl ExecutableTxIterator<Self>, Self::Error> {
        self.inner.tx_iterator_for_payload(payload)
    }
}

#[derive(Debug, Clone)]
struct SettlingExecutorFactory {
    inner: EthBlockExecutorFactory<RethReceiptBuilder, Arc<ChainSpec>, RethEvmFactory>,
}

type SettlingEvm<DB, I> = <RethEvmFactory as EvmFactory>::Evm<DB, I>;

type SettlingTxResult = EthTxResult<HaltReason, TxType>;

impl BlockExecutorFactory for SettlingExecutorFactory {
    type EvmFactory = RethEvmFactory;
    type TxExecutionResult = SettlingTxResult;
    type ExecutionCtx<'a> = EthBlockExecutionCtx<'a>;
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type Executor<'a, DB: StateDB, I: Inspector<EthEvmContext<DB>>> = SettlingExecutor<'a, DB, I>;

    fn evm_factory(&self) -> &Self::EvmFactory {
        self.inner.evm_factory()
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: SettlingEvm<DB, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> Self::Executor<'a, DB, I>
    where
        DB: StateDB,
        I: Inspector<EthEvmContext<DB>>,
    {
        SettlingExecutor {
            inner: EthBlockExecutor::new(evm, ctx, self.inner.spec(), self.inner.receipt_builder()),
        }
    }
}

type InnerExecutor<'a, DB, I> =
    EthBlockExecutor<'a, SettlingEvm<DB, I>, &'a Arc<ChainSpec>, &'a RethReceiptBuilder>;

/// Ethereum block executor that credits `SETTLEMENT` to the sender in each transaction's
/// uncommitted state, like a chain settling a per-transaction refund outside the EVM.
struct SettlingExecutor<'a, DB: StateDB, I: Inspector<EthEvmContext<DB>>> {
    inner: InnerExecutor<'a, DB, I>,
}

impl<DB, I> BlockExecutor for SettlingExecutor<'_, DB, I>
where
    DB: StateDB,
    I: Inspector<EthEvmContext<DB>>,
{
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type Evm = SettlingEvm<DB, I>;
    type Result = SettlingTxResult;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::Result, BlockExecutionError> {
        let (tx_env, tx) = tx.into_parts();
        let sender = *tx.signer();

        let mut output = self.inner.execute_transaction_without_commit((tx_env, tx))?;
        let account =
            output.result.state.get_mut(&sender).expect("every transaction loads its sender");
        account.info.balance += SETTLEMENT;

        Ok(output)
    }

    fn commit_transaction(&mut self, output: Self::Result) -> GasOutput {
        self.inner.commit_transaction(output)
    }

    fn finish(
        self,
    ) -> Result<(Self::Evm, BlockExecutionResult<Self::Receipt>), BlockExecutionError> {
        self.inner.finish()
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }

    fn receipts(&self) -> &[Self::Receipt] {
        self.inner.receipts()
    }
}
