//! Firehose block executor that traces EVM execution and emits FIRE lines.

use std::fmt::Debug;

use crate::{inspector::FirehoseInspector, mapper, mapper::SignatureFields};
use alloy_consensus::{transaction::TxHashRef, BlockHeader, Transaction, TxReceipt};
use alloy_evm::block::BlockExecutor as _;
use alloy_primitives::{Log, Sealable, U256};
use reth_evm::{
    execute::{BlockExecutionError, Executor},
    ConfigureEvm, Evm as _, OnStateHook,
};
use reth_execution_types::BlockExecutionResult;
use reth_node_api::NodePrimitives;
use reth_primitives_traits::{Block as BlockTrait, BlockBody, BlockTy, RecoveredBlock, TxTy};
use reth_revm::{db::states::bundle_state::BundleRetention, Database as _, State};

/// A block executor that wraps a [`ConfigureEvm`] and fires Firehose tracer lifecycle
/// hooks (on_block_start, on_tx_start, on_tx_end, on_block_end) around every block
/// execution when the global tracer is initialized.
///
/// When the global tracer is **not** initialized it falls back transparently to the
/// inner executor's normal `execute_one` path, so there is zero overhead when
/// Firehose tracing is disabled.
pub struct FirehoseBlockExecutor<F, DB> {
    /// The underlying EVM configuration used to create block executors.
    pub strategy_factory: F,
    /// Revm state holding all accumulated changes across the batch.
    pub db: State<DB>,
}

impl Debug for FirehoseBlockExecutor<(), ()> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirehoseBlockExecutor").finish()
    }
}

impl<F, DB: reth_evm::Database> FirehoseBlockExecutor<F, DB> {
    /// Creates a new `FirehoseBlockExecutor`.
    pub fn new(strategy_factory: F, db: DB) -> Self {
        let db = State::builder().with_database(db).with_bundle_update().build();
        Self { strategy_factory, db }
    }
}

impl<F, DB> Executor<DB> for FirehoseBlockExecutor<F, DB>
where
    F: ConfigureEvm,
    DB: reth_evm::Database,
    BlockTy<F::Primitives>: BlockTrait,
    <BlockTy<F::Primitives> as BlockTrait>::Header: BlockHeader + Sealable,
    <BlockTy<F::Primitives> as BlockTrait>::Body: BlockBody,
    <<BlockTy<F::Primitives> as BlockTrait>::Body as BlockBody>::OmmerHeader:
        BlockHeader + Sealable,
    TxTy<F::Primitives>: Transaction + TxHashRef + SignatureFields,
{
    type Primitives = F::Primitives;
    type Error = BlockExecutionError;

    fn execute_one(
        &mut self,
        block: &RecoveredBlock<<Self::Primitives as NodePrimitives>::Block>,
    ) -> Result<BlockExecutionResult<<Self::Primitives as NodePrimitives>::Receipt>, Self::Error>
    {
        let result = self
            .strategy_factory
            .executor_for_block(&mut self.db, block)
            .map_err(BlockExecutionError::other)?
            .execute_block(block.transactions_recovered())?;
        self.db.merge_transitions(BundleRetention::Reverts);
        Ok(result)
    }

    fn execute_and_trace_one(
        &mut self,
        block: &RecoveredBlock<<Self::Primitives as NodePrimitives>::Block>,
    ) -> Result<BlockExecutionResult<<Self::Primitives as NodePrimitives>::Receipt>, Self::Error>
    {
        if crate::GLOBAL_TRACER.get().is_none() {
            return Err(BlockExecutionError::msg(
                "FirehoseBlockExecutor requires the global tracer to be initialized for execute_and_trace_one",
            ));
        }

        trace_block_with_inspector(&self.strategy_factory, &mut self.db, block)
    }

    fn execute_one_with_state_hook<H>(
        &mut self,
        block: &RecoveredBlock<<Self::Primitives as NodePrimitives>::Block>,
        state_hook: H,
    ) -> Result<BlockExecutionResult<<Self::Primitives as NodePrimitives>::Receipt>, Self::Error>
    where
        H: OnStateHook + 'static,
    {
        let result = self
            .strategy_factory
            .executor_for_block(&mut self.db, block)
            .map_err(BlockExecutionError::other)?
            .with_state_hook(Some(Box::new(state_hook)))
            .execute_block(block.transactions_recovered())?;
        self.db.merge_transitions(BundleRetention::Reverts);
        Ok(result)
    }

    fn into_state(self) -> State<DB> {
        self.db
    }

    fn size_hint(&self) -> usize {
        self.db.bundle_state.size_hint()
    }
}

/// Executes a block with a `FirehoseInspector`, calling the full tracer lifecycle:
/// `on_block_start` / `on_genesis_block`, per-tx `on_tx_start` / `on_tx_end`,
/// and `on_block_end`. Mirrors the pattern in `runner::trace_block` but operates
/// on an arbitrary `DB` without requiring an `ExExContext`.
pub fn trace_block_with_inspector<F, DB>(
    evm_config: &F,
    db: &mut State<DB>,
    block: &RecoveredBlock<<F::Primitives as NodePrimitives>::Block>,
) -> Result<BlockExecutionResult<<F::Primitives as NodePrimitives>::Receipt>, BlockExecutionError>
where
    F: ConfigureEvm,
    DB: reth_evm::Database,
    BlockTy<F::Primitives>: BlockTrait,
    <BlockTy<F::Primitives> as BlockTrait>::Header: BlockHeader + Sealable,
    <BlockTy<F::Primitives> as BlockTrait>::Body: BlockBody,
    <<BlockTy<F::Primitives> as BlockTrait>::Body as BlockBody>::OmmerHeader:
        BlockHeader + Sealable,
    TxTy<F::Primitives>: Transaction + TxHashRef + SignatureFields,
{
    let tracer = &mut *crate::tracer();

    // Block 1 is the first real block after genesis; emit the genesis-block marker
    // (which includes the genesis alloc) and execute normally.  In the ExEx runner
    // the genesis block short-circuits here, but in the execution stage we still need
    // to process the transactions, so we fall through after emitting the event.
    if block.number() == 1 {
        tracer.on_genesis_block(
            firehose_tracer::types::BlockEvent {
                block: mapper::to_block_data_eth::<F::Primitives>(block),
                finalized: None,
                flash_block: None,
            },
            Default::default(),
        );
        // Still execute the block so state changes are applied.
        let result = evm_config
            .executor_for_block(db, block)
            .map_err(BlockExecutionError::other)?
            .execute_block(block.transactions_recovered())?;
        db.merge_transitions(BundleRetention::Reverts);
        return Ok(result);
    }

    tracer.on_block_start(firehose_tracer::types::BlockEvent {
        block: mapper::to_block_data_eth::<F::Primitives>(block),
        finalized: None,
        flash_block: None,
    });

    let evm_env = evm_config.evm_env(block.header()).map_err(BlockExecutionError::other)?;
    let exec_ctx =
        evm_config.context_for_block(block.sealed_block()).map_err(BlockExecutionError::other)?;

    // Inspector borrows tracer mutably for the duration of the block execution.
    let inspector = FirehoseInspector::new(tracer);
    let evm = evm_config.evm_with_env_and_inspector(&mut *db, evm_env, inspector);
    let mut executor = evm_config.create_executor(evm, exec_ctx);

    // System calls (EIP-4788, EIP-2935, etc.)
    executor.evm_mut().inspector_mut().tracer_mut().on_system_call_start();
    executor.apply_pre_execution_changes().map_err(BlockExecutionError::from)?;
    executor.evm_mut().inspector_mut().tracer_mut().on_system_call_end();

    let mut prev_cumulative_gas: u64 = 0;
    let mut log_index: u32 = 0;

    for (tx_index, recovered_tx) in block.transactions_recovered().enumerate() {
        let tx: &TxTy<F::Primitives> = &**recovered_tx;

        let (r, s, v) = tx.signature_fields();
        let tx_event = mapper::signed_tx_to_tx_event(tx, recovered_tx.signer(), tx_index, r, s, v);
        executor.evm_mut().inspector_mut().tracer_mut().on_tx_start(tx_event, None);

        let tx_result = executor.execute_transaction_without_commit(recovered_tx).map_err(|e| {
            BlockExecutionError::msg(format!("transaction execution failed: {e:?}"))
        })?;

        let receipt_data = {
            use reth_evm::block::TxResult as _;
            let result = &tx_result.result().result;
            let gas_used = result.tx_gas_used();
            let cumulative_gas = prev_cumulative_gas + gas_used;

            let sender = recovered_tx.signer();
            let coinbase = block.header().beneficiary();
            let base_fee = block.header().base_fee_per_gas().unwrap_or(0);
            let effective_gas_price: u128 = if tx.is_dynamic_fee() {
                std::cmp::min(
                    tx.max_fee_per_gas(),
                    base_fee as u128 + tx.max_priority_fee_per_gas().unwrap_or(0),
                )
            } else {
                tx.gas_price().unwrap_or(0)
            };

            let (evm_db, inspector, _) = executor.evm_mut().components_mut();
            inspector.process_post_tx_balance_changes(
                sender,
                coinbase,
                tx.gas_limit(),
                gas_used,
                effective_gas_price,
                base_fee,
                |addr| {
                    evm_db.basic(addr).ok().flatten().map(|info| info.balance).unwrap_or(U256::ZERO)
                },
            );

            let rd = mapper::receipt_data_from_parts(
                tx_index as u32,
                gas_used,
                cumulative_gas,
                result.is_success() as u64,
                result.logs(),
                log_index,
            );
            let log_count = result.logs().len() as u32;
            prev_cumulative_gas = cumulative_gas;
            log_index += log_count;
            rd
        };

        executor
            .commit_transaction(tx_result)
            .map_err(|e| BlockExecutionError::msg(format!("transaction commit failed: {e:?}")))?;

        executor.evm_mut().inspector_mut().tracer_mut().on_tx_end(Some(&receipt_data), None);
    }

    executor.evm_mut().inspector_mut().tracer_mut().on_system_call_start();

    // apply_post_execution_changes consumes the executor, dropping the inspector and
    // releasing the mutable borrow on the tracer.
    let result = executor.apply_post_execution_changes().map_err(BlockExecutionError::from)?;

    // Tracer borrow released — safe to call directly.
    tracer.on_system_call_end();
    tracer.on_block_end(None);

    db.merge_transitions(BundleRetention::Reverts);
    Ok(result)
}

/// Like [`trace_block_with_inspector`] but also accepts the receipts produced by the first
/// (validation) execution so that `on_tx_end` receives accurate gas-used, log, and status
/// data.  Additionally uses actual transaction signatures via [`SignatureFields`].
///
/// This is the entry-point for **live-block** tracing from the engine payload validator,
/// where both the `RecoveredBlock` (with real signatures) and the execution receipts are
/// already available from the preceding validation pass.
pub fn trace_live_block<F, DB, R>(
    evm_config: &F,
    db: &mut State<DB>,
    block: &RecoveredBlock<<F::Primitives as NodePrimitives>::Block>,
    receipts: &[R],
) -> Result<(), BlockExecutionError>
where
    F: ConfigureEvm,
    DB: reth_evm::Database,
    BlockTy<F::Primitives>: BlockTrait,
    <BlockTy<F::Primitives> as BlockTrait>::Header: BlockHeader + Sealable,
    <BlockTy<F::Primitives> as BlockTrait>::Body: BlockBody,
    <<BlockTy<F::Primitives> as BlockTrait>::Body as BlockBody>::OmmerHeader:
        BlockHeader + Sealable,
    TxTy<F::Primitives>: Transaction + TxHashRef + SignatureFields,
    R: TxReceipt<Log = Log>,
{
    let tracer = &mut *crate::tracer();

    if block.number() == 1 {
        tracer.on_genesis_block(
            firehose_tracer::types::BlockEvent {
                block: mapper::to_block_data_eth::<F::Primitives>(block),
                finalized: None,
                flash_block: None,
            },
            Default::default(),
        );
        return Ok(());
    }

    tracer.on_block_start(firehose_tracer::types::BlockEvent {
        block: mapper::to_block_data_eth::<F::Primitives>(block),
        finalized: None,
        flash_block: None,
    });

    let evm_env = evm_config.evm_env(block.header()).map_err(BlockExecutionError::other)?;
    let exec_ctx =
        evm_config.context_for_block(block.sealed_block()).map_err(BlockExecutionError::other)?;

    let inspector = FirehoseInspector::new(tracer);
    let evm = evm_config.evm_with_env_and_inspector(&mut *db, evm_env, inspector);
    let mut executor = evm_config.create_executor(evm, exec_ctx);

    executor.evm_mut().inspector_mut().tracer_mut().on_system_call_start();
    executor.apply_pre_execution_changes().map_err(BlockExecutionError::from)?;
    executor.evm_mut().inspector_mut().tracer_mut().on_system_call_end();

    let mut prev_cumulative_gas: u64 = 0;
    let mut log_index: u32 = 0;

    for (tx_index, (recovered_tx, receipt)) in
        block.transactions_recovered().zip(receipts.iter()).enumerate()
    {
        let tx: &TxTy<F::Primitives> = &**recovered_tx;
        let (r, s, v) = tx.signature_fields();
        let tx_event = mapper::signed_tx_to_tx_event(tx, recovered_tx.signer(), tx_index, r, s, v);
        executor.evm_mut().inspector_mut().tracer_mut().on_tx_start(tx_event, None);

        let tx_result = executor.execute_transaction_without_commit(recovered_tx).map_err(|e| {
            BlockExecutionError::msg(format!("transaction execution failed: {e:?}"))
        })?;

        {
            let result_gas_used = {
                use reth_evm::block::TxResult as _;
                tx_result.result().result.tx_gas_used()
            };
            let sender = recovered_tx.signer();
            let coinbase = block.header().beneficiary();
            let gas_limit = tx.gas_limit();
            let base_fee = block.header().base_fee_per_gas().unwrap_or(0);
            let effective_gas_price: u128 = if tx.is_dynamic_fee() {
                std::cmp::min(
                    tx.max_fee_per_gas(),
                    base_fee as u128 + tx.max_priority_fee_per_gas().unwrap_or(0),
                )
            } else {
                tx.gas_price().unwrap_or(0)
            };

            let (evm_db, inspector, _) = executor.evm_mut().components_mut();
            inspector.process_post_tx_balance_changes(
                sender,
                coinbase,
                gas_limit,
                result_gas_used,
                effective_gas_price,
                base_fee,
                |addr| {
                    evm_db.basic(addr).ok().flatten().map(|info| info.balance).unwrap_or(U256::ZERO)
                },
            );
        }

        executor
            .commit_transaction(tx_result)
            .map_err(|e| BlockExecutionError::msg(format!("transaction commit failed: {e:?}")))?;

        let cumulative_gas = receipt.cumulative_gas_used();
        let gas_used = cumulative_gas - prev_cumulative_gas;
        let log_count = receipt.logs().len() as u32;
        let receipt_data = mapper::to_receipt_data(receipt, tx_index as u32, gas_used, log_index);
        prev_cumulative_gas = cumulative_gas;
        log_index += log_count;

        executor.evm_mut().inspector_mut().tracer_mut().on_tx_end(Some(&receipt_data), None);
    }

    executor.evm_mut().inspector_mut().tracer_mut().on_system_call_start();

    executor.apply_post_execution_changes().map_err(BlockExecutionError::from)?;

    tracer.on_system_call_end();
    tracer.on_block_end(None);

    db.merge_transitions(BundleRetention::Reverts);
    Ok(())
}

// ---------------------------------------------------------------------------
// FirehoseEvmConfig
// ---------------------------------------------------------------------------

/// A thin wrapper around any [`ConfigureEvm`] that overrides [`ConfigureEvm::batch_executor`]
/// to return a [`FirehoseBlockExecutor`] instead of the default `BasicBlockExecutor`.
///
/// All other methods delegate directly to the inner config.
#[derive(Clone, Debug)]
pub struct FirehoseEvmConfig<F> {
    /// The wrapped EVM configuration.
    pub inner: F,
}

impl<F: ConfigureEvm> FirehoseEvmConfig<F> {
    /// Wraps an existing EVM configuration.
    pub fn new(inner: F) -> Self {
        Self { inner }
    }
}

impl<F> ConfigureEvm for FirehoseEvmConfig<F>
where
    F: ConfigureEvm,
    BlockTy<F::Primitives>: BlockTrait,
    <BlockTy<F::Primitives> as BlockTrait>::Header: BlockHeader + Sealable,
    <BlockTy<F::Primitives> as BlockTrait>::Body: BlockBody,
    <<BlockTy<F::Primitives> as BlockTrait>::Body as BlockBody>::OmmerHeader:
        BlockHeader + Sealable,
    TxTy<F::Primitives>: Transaction + TxHashRef + SignatureFields,
{
    type Primitives = F::Primitives;
    type Error = F::Error;
    type NextBlockEnvCtx = F::NextBlockEnvCtx;
    type BlockExecutorFactory = F::BlockExecutorFactory;
    type BlockAssembler = F::BlockAssembler;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        self.inner.block_executor_factory()
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        self.inner.block_assembler()
    }

    fn evm_env(
        &self,
        header: &reth_primitives_traits::HeaderTy<Self::Primitives>,
    ) -> Result<reth_evm::EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env(header)
    }

    fn next_evm_env(
        &self,
        parent: &reth_primitives_traits::HeaderTy<Self::Primitives>,
        attributes: &Self::NextBlockEnvCtx,
    ) -> Result<reth_evm::EvmEnvFor<Self>, Self::Error> {
        self.inner.next_evm_env(parent, attributes)
    }

    fn context_for_block<'a>(
        &self,
        block: &'a reth_primitives_traits::SealedBlock<
            reth_primitives_traits::BlockTy<Self::Primitives>,
        >,
    ) -> Result<reth_evm::ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_block(block)
    }

    fn context_for_next_block(
        &self,
        parent: &reth_primitives_traits::SealedHeader<
            reth_primitives_traits::HeaderTy<Self::Primitives>,
        >,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<reth_evm::ExecutionCtxFor<'_, Self>, Self::Error> {
        self.inner.context_for_next_block(parent, attributes)
    }

    fn batch_executor<DB: reth_evm::Database>(
        &self,
        db: DB,
    ) -> impl Executor<DB, Primitives = Self::Primitives, Error = BlockExecutionError> {
        FirehoseBlockExecutor::new(self.inner.clone(), db)
    }
}

impl<F, ExecutionData> reth_evm::ConfigureEngineEvm<ExecutionData> for FirehoseEvmConfig<F>
where
    F: reth_evm::ConfigureEngineEvm<ExecutionData>,
    BlockTy<F::Primitives>: BlockTrait,
    <BlockTy<F::Primitives> as BlockTrait>::Header: BlockHeader + Sealable,
    <BlockTy<F::Primitives> as BlockTrait>::Body: BlockBody,
    <<BlockTy<F::Primitives> as BlockTrait>::Body as BlockBody>::OmmerHeader:
        BlockHeader + Sealable,
    TxTy<F::Primitives>: Transaction + TxHashRef + SignatureFields,
{
    fn evm_env_for_payload(
        &self,
        payload: &ExecutionData,
    ) -> Result<reth_evm::EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env_for_payload(payload)
    }

    fn context_for_payload<'a>(
        &self,
        payload: &'a ExecutionData,
    ) -> Result<reth_evm::ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_payload(payload)
    }

    fn tx_iterator_for_payload(
        &self,
        payload: &ExecutionData,
    ) -> Result<impl reth_evm::ExecutableTxIterator<Self>, Self::Error> {
        self.inner.tx_iterator_for_payload(payload)
    }
}
