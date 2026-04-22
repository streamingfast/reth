//! Firehose block executor that traces EVM execution and emits FIRE lines.

use std::{collections::HashMap, fmt::Debug};

use crate::{block_tracer::FirehoseBlockTracer, mapper, mapper::SignatureFields};
use alloy_consensus::{transaction::TxHashRef, BlockHeader, Transaction, TxReceipt};
use alloy_eips::eip4895::Withdrawals;
use alloy_evm::block::BlockExecutor as _;
use alloy_primitives::{Address, Log, Sealable, U256};
use reth_evm::{
    execute::{BlockExecutionError, Executor},
    ConfigureEvm, Evm as _, OnStateHook,
};
use reth_execution_types::BlockExecutionResult;
use reth_node_api::NodePrimitives;
use reth_primitives_traits::{Block as BlockTrait, BlockBody, BlockTy, RecoveredBlock, TxTy};
use reth_revm::{db::states::bundle_state::BundleRetention, Database as _, State};

/// A block executor that wraps a [`ConfigureEvm`] and, when the global tracer is
/// initialized, fires Firehose tracer hooks around every block execution.
///
/// ## Post-validation flush deferral (pipeline path)
///
/// [`Self::execute_and_trace_one`] emits `on_block_start` and all mid-block events for
/// a block, but **does not flush** (`on_block_end(None)`) until one of:
///
/// * The next call to `execute_and_trace_one` on the same executor — which means the
///   previous block's `validate_block_post_execution` (called by the caller between
///   the two invocations) succeeded, since an error there aborts the batch.
/// * [`Executor::into_state`] — which means the batch completed without any validation
///   error, so the last block is also safe to flush.
///
/// If the executor is dropped without reaching either point (e.g. validation error
/// short-circuits the caller), the stashed [`FirehoseBlockTracer`] is dropped instead,
/// which emits `on_block_end(Some(err))` and discards the block so invalid blocks are
/// never flushed downstream.
pub struct FirehoseBlockExecutor<F, DB> {
    /// The underlying EVM configuration used to create block executors.
    pub strategy_factory: F,
    /// Revm state holding all accumulated changes across the batch.
    pub db: State<DB>,
    /// Tracer guard for the most recently executed block. Finalized by the next
    /// `execute_and_trace_one` call or by `into_state` — see the struct-level docs.
    pending_tracer: Option<FirehoseBlockTracer>,
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
        Self { strategy_factory, db, pending_tracer: None }
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

    /// Traces a block and defers `on_block_end(None)` until the caller has validated
    /// the result.
    ///
    /// Reaching this call means the previous block (if any) passed its validation —
    /// otherwise the caller would have returned an error rather than looping back in —
    /// so we flush the previous block's `on_block_end(None)` here before starting the
    /// next one. The new block's guard is stashed in `self.pending_tracer` and finalized
    /// either by the next call to this method or by [`Self::into_state`]. If the caller
    /// drops this executor without reaching either point (e.g. post-execution validation
    /// fails), the stashed guard is dropped, which emits `on_block_end(Some(err))` and
    /// discards the block.
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

        if let Some(prev) = self.pending_tracer.take() {
            prev.mark_verified();
        }

        // In the pipeline / staged sync path, the block we're about to trace is by
        // definition already finalized — staged sync only replays blocks up to the
        // finalized head — so advertise it as the finalized ref in the block event.
        let finalized = Some(firehose_tracer::types::FinalizedBlockRef {
            number: block.number(),
            hash: Some(block.hash()),
        });
        let mut tracer = FirehoseBlockTracer::start::<F::Primitives>(block, finalized);
        match trace_block::<F, DB, <F::Primitives as NodePrimitives>::Receipt>(
            &self.strategy_factory,
            &mut self.db,
            block,
            None,
            &mut tracer,
        ) {
            Ok(result) => {
                self.pending_tracer = Some(tracer);
                Ok(result)
            }
            Err(e) => {
                tracer.mark_failed(&e);
                Err(e)
            }
        }
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

    fn into_state(mut self) -> State<DB> {
        // Reaching `into_state` without an error means the batch completed successfully —
        // including post-execution validation on the most recent block — so finalize any
        // pending tracer as verified rather than letting Drop discard it.
        if let Some(pending) = self.pending_tracer.take() {
            pending.mark_verified();
        }
        self.db
    }

    fn size_hint(&self) -> usize {
        self.db.bundle_state.size_hint()
    }
}

/// Executes a block with a [`crate::inspector::FirehoseInspector`] sourced from the provided
/// [`FirehoseBlockTracer`], firing per-transaction and system-call tracer hooks.
///
/// This function is shared by both the pipeline ([`FirehoseBlockExecutor::execute_and_trace_one`])
/// and the live engine path ([`crate::engine`], via payload validation). It deliberately does **not**
/// emit `on_block_start` or `on_block_end` — those are owned by the caller via
/// [`FirehoseBlockTracer::start`], [`FirehoseBlockTracer::mark_verified`] and
/// [`FirehoseBlockTracer::mark_failed`] — so that callers can defer the end-of-block signal until
/// after post-execution validation (receipt root, state root, consensus) has completed.
///
/// When `receipts` is `Some`, the tracer emits `on_tx_end` with receipts computed from the
/// caller-provided slice (live path: receipts already validated by the preceding execution pass).
/// When `None`, the tracer reconstructs them from each transaction's execution result
/// (pipeline path: no prior receipts available).
///
/// State mutations (pre/post-execution changes, tx commits) are merged into `db` with
/// [`BundleRetention::Reverts`] on success. On error, the bundle is left as-is so the caller can
/// observe partial state for diagnostics.
pub fn trace_block<F, DB, R>(
    evm_config: &F,
    db: &mut State<DB>,
    block: &RecoveredBlock<<F::Primitives as NodePrimitives>::Block>,
    receipts: Option<&[R]>,
    tracer_guard: &mut FirehoseBlockTracer,
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
    R: TxReceipt<Log = Log>,
{
    // Block 1 is the genesis marker: the live caller short-circuits here because the validation
    // pass already produced receipts. The pipeline caller still has real transactions to execute,
    // so fall through in that case. We distinguish by the presence of pre-computed receipts.
    if tracer_guard.is_genesis() && receipts.is_some() {
        return Ok(BlockExecutionResult {
            receipts: Vec::new(),
            requests: Default::default(),
            gas_used: 0,
            blob_gas_used: 0,
        });
    }

    // Run all executor logic inside a helper function. When the helper returns (whether
    // Ok or Err), its locals — including the executor and the FirehoseInspector that
    // mutably borrows the tracer — are dropped. The tracer borrow is released before
    // we touch `tracer_guard.tracer_mut()` again for post-execution emissions.
    let block_result = execute_block_inner(tracer_guard, evm_config, db, block, receipts);

    match block_result {
        Ok(result) => {
            // Tracer borrow released — safe to call directly.
            // Close the post-execution system-call window FIRST so that
            // self.transaction is None, then emit withdrawal balance changes so
            // on_balance_change routes them to block.balance_changes (not
            // deferred_call_state, which would be discarded by reset_transaction).
            let tracer = tracer_guard.tracer_mut();
            tracer.on_system_call_end();
            emit_withdrawal_balance_changes(tracer, db, block.body().withdrawals());
            db.merge_transitions(BundleRetention::Reverts);
            Ok(result)
        }
        Err(e) => Err(e),
    }
}

/// Core executor logic for [`trace_block`]. All locals (executor, inspector) are dropped on
/// return so the mutable borrow on the tracer is released before the caller emits any further
/// post-execution events.
#[allow(clippy::too_many_arguments)]
fn execute_block_inner<F, DB, R>(
    tracer_guard: &mut FirehoseBlockTracer,
    evm_config: &F,
    db: &mut State<DB>,
    block: &RecoveredBlock<<F::Primitives as NodePrimitives>::Block>,
    receipts: Option<&[R]>,
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
    R: TxReceipt<Log = Log>,
{
    let evm_env = evm_config.evm_env(block.header()).map_err(BlockExecutionError::other)?;
    let exec_ctx =
        evm_config.context_for_block(block.sealed_block()).map_err(BlockExecutionError::other)?;

    let inspector = tracer_guard.inspector();
    let evm = evm_config.evm_with_env_and_inspector(db, evm_env, inspector);
    let mut executor = evm_config.create_executor(evm, exec_ctx);

    // System calls (EIP-4788, EIP-2935, etc.)
    executor.evm_mut().inspector_mut().tracer_mut().on_system_call_start();
    executor.apply_pre_execution_changes().map_err(BlockExecutionError::from)?;
    executor.evm_mut().inspector_mut().tracer_mut().on_system_call_end();

    let mut prev_cumulative_gas: u64 = 0;
    let mut log_index: u32 = 0;

    // EIP-4844: blob gas price is a block-level property derived from excess_blob_gas.
    // None for pre-Cancun blocks that don't have this field.
    let blob_gas_price: Option<U256> = block
        .header()
        .excess_blob_gas()
        .map(|excess| U256::from(alloy_eips::eip4844::calc_blob_gasprice(excess)));

    for (tx_index, recovered_tx) in block.transactions_recovered().enumerate() {
        let tx: &TxTy<F::Primitives> = &**recovered_tx;

        let (r, s, v) = tx.signature_fields();
        let tx_event = mapper::signed_tx_to_tx_event(tx, recovered_tx.signer(), tx_index, r, s, v);
        executor.evm_mut().inspector_mut().tracer_mut().on_tx_start(tx_event, None);

        let tx_result = executor.execute_transaction_without_commit(recovered_tx).map_err(|e| {
            BlockExecutionError::msg(format!("transaction execution failed: {e:?}"))
        })?;

        // Compute gas used and effective gas price, then emit post-tx balance changes (sender
        // refund, miner fee). Both paths (caller-supplied receipts vs reconstructed-from-tx_result)
        // use the same inspector bookkeeping.
        let gas_used = {
            use reth_evm::block::TxResult as _;
            tx_result.result().result.tx_gas_used()
        };
        {
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
                gas_used,
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

        // Build receipt_data for on_tx_end. If the caller supplied receipts (live path), use
        // them for accurate cumulative gas and log data; otherwise reconstruct from the
        // tx_result we just observed (pipeline path).
        let mut receipt_data = if let Some(receipts) = receipts {
            let receipt = &receipts[tx_index];
            let cumulative_gas = receipt.cumulative_gas_used();
            let actual_gas_used = cumulative_gas - prev_cumulative_gas;
            let rd = mapper::to_receipt_data(receipt, tx_index as u32, actual_gas_used, log_index);
            log_index += receipt.logs().len() as u32;
            prev_cumulative_gas = cumulative_gas;
            rd
        } else {
            let committed =
                executor.receipts().last().expect("commit_transaction pushed a receipt");
            let rd = mapper::to_receipt_data(committed, tx_index as u32, gas_used, log_index);
            log_index += committed.logs().len() as u32;
            prev_cumulative_gas += gas_used;
            rd
        };

        // EIP-4844: populate blob gas fields for type-3 transactions.
        if let Some(blob_gas_used) = tx.blob_gas_used() {
            receipt_data.blob_gas_used = blob_gas_used;
            receipt_data.blob_gas_price = blob_gas_price;
        }

        executor.evm_mut().inspector_mut().tracer_mut().on_tx_end(Some(&receipt_data), None);
    }

    executor.evm_mut().inspector_mut().tracer_mut().on_system_call_start();

    // apply_post_execution_changes takes self, consuming the executor and dropping the
    // inspector. The tracer borrow is released when this function returns.
    executor.apply_post_execution_changes().map_err(BlockExecutionError::from)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Emits withdrawal balance changes to the tracer after the post-execution system-call window
/// has been closed (i.e. after `on_system_call_end` has been called).
///
/// EIP-4895 validator withdrawals are applied via `db.increment_balances()` inside the executor's
/// `finish()` call, bypassing the EVM journal entirely. The `FirehoseInspector` never sees them.
/// This function bridges the gap by replaying withdrawals in order against the final DB state.
///
/// Must be called with `tracer.transaction == None` (i.e. outside any system-call window) so
/// that `on_balance_change` routes the events to `block.balance_changes` directly. Calling it
/// inside the system-call window causes the changes to land in `deferred_call_state`, which is
/// discarded by `on_system_call_end`.
fn emit_withdrawal_balance_changes<DB>(
    tracer: &mut firehose_tracer::Tracer,
    db: &mut State<DB>,
    withdrawals: Option<&Withdrawals>,
) where
    DB: reth_evm::Database,
{
    use firehose_tracer::pb::sf::ethereum::r#type::v2::balance_change::Reason;

    let Some(withdrawals) = withdrawals else { return };
    let withdrawals = withdrawals.as_slice();
    if withdrawals.is_empty() {
        return;
    }

    let events = withdrawal_balance_events(withdrawals, |addr| {
        db.basic(addr).ok().flatten().map(|i| i.balance).unwrap_or_default()
    });

    for (addr, pre, post) in events {
        tracer.on_balance_change(addr, pre, post, Reason::Withdrawal);
    }
}

/// Computes per-withdrawal balance change events in forward order by replaying backwards
/// from the known final balances.
///
/// Starting from the post-all-withdrawals balance for each address (fetched via
/// `get_final_balance`), the function walks the withdrawal list in reverse to reconstruct
/// the pre/post values for each individual withdrawal, then returns the events in the
/// original forward order so callers emit them with monotonically increasing ordinals.
///
/// Withdrawals with `amount == 0` are skipped (they produce no balance change).
fn withdrawal_balance_events(
    withdrawals: &[alloy_eips::eip4895::Withdrawal],
    mut get_final_balance: impl FnMut(Address) -> U256,
) -> Vec<(Address, U256, U256)> {
    // Seed the running balance map with the final DB value for every address that appears.
    let mut current: HashMap<Address, U256> = HashMap::new();
    for w in withdrawals {
        if w.amount > 0 {
            current.entry(w.address).or_insert_with(|| get_final_balance(w.address));
        }
    }

    // Walk backwards, peeling off each withdrawal to recover the per-step pre/post pair.
    let mut events: Vec<(Address, U256, U256)> = Vec::with_capacity(withdrawals.len());
    for w in withdrawals.iter().rev() {
        if w.amount == 0 {
            continue;
        }
        let post = *current.get(&w.address).unwrap_or(&U256::ZERO);
        let pre = post.saturating_sub(w.amount_wei());
        events.push((w.address, pre, post));
        current.insert(w.address, pre);
    }

    events.reverse();
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_eips::eip4895::Withdrawal;
    use alloy_primitives::Address;

    fn addr(byte: u8) -> Address {
        Address::repeat_byte(byte)
    }

    fn gwei(n: u64) -> u64 {
        n
    }

    fn wei(gwei: u64) -> U256 {
        U256::from(gwei) * U256::from(1_000_000_000u64)
    }

    fn make_withdrawal(index: u64, validator: u64, address: Address, amount_gwei: u64) -> Withdrawal {
        Withdrawal { index, validator_index: validator, address, amount: amount_gwei }
    }

    /// Calls `withdrawal_balance_events` with a fixed balance map.
    fn events_with_balances(
        withdrawals: &[Withdrawal],
        final_balances: &HashMap<Address, U256>,
    ) -> Vec<(Address, U256, U256)> {
        withdrawal_balance_events(withdrawals, |addr| {
            final_balances.get(&addr).copied().unwrap_or_default()
        })
    }

    #[test]
    fn test_single_withdrawal() {
        let a = addr(0xAA);
        let ws = [make_withdrawal(0, 0, a, gwei(100))];
        let mut finals = HashMap::new();
        finals.insert(a, wei(1100)); // started at 1000, received 100 gwei

        let events = events_with_balances(&ws, &finals);

        assert_eq!(events, vec![(a, wei(1000), wei(1100))]);
    }

    #[test]
    fn test_two_withdrawals_same_address_ordering() {
        // W1: A +50 gwei, W2: A +150 gwei → final A = 1200
        let a = addr(0xAA);
        let ws = [
            make_withdrawal(0, 0, a, gwei(50)),
            make_withdrawal(1, 0, a, gwei(150)),
        ];
        let mut finals = HashMap::new();
        finals.insert(a, wei(1200)); // started at 1000

        let events = events_with_balances(&ws, &finals);

        assert_eq!(events.len(), 2);
        // W1: pre=1000, post=1050
        assert_eq!(events[0], (a, wei(1000), wei(1050)));
        // W2: pre=1050, post=1200
        assert_eq!(events[1], (a, wei(1050), wei(1200)));
    }

    #[test]
    fn test_interleaved_different_addresses() {
        // W1: A +50, W2: B +20, W3: A +150
        // Final: A=1200, B=220
        let a = addr(0xAA);
        let b = addr(0xBB);
        let ws = [
            make_withdrawal(0, 0, a, gwei(50)),
            make_withdrawal(1, 1, b, gwei(20)),
            make_withdrawal(2, 0, a, gwei(150)),
        ];
        let mut finals = HashMap::new();
        finals.insert(a, wei(1200)); // started at 1000
        finals.insert(b, wei(220));  // started at 200

        let events = events_with_balances(&ws, &finals);

        assert_eq!(events.len(), 3);
        assert_eq!(events[0], (a, wei(1000), wei(1050))); // W1
        assert_eq!(events[1], (b, wei(200),  wei(220)));  // W2
        assert_eq!(events[2], (a, wei(1050), wei(1200))); // W3
    }

    #[test]
    fn test_zero_amount_withdrawals_skipped() {
        let a = addr(0xAA);
        let ws = [
            make_withdrawal(0, 0, a, 0),        // skipped
            make_withdrawal(1, 0, a, gwei(100)),
        ];
        let mut finals = HashMap::new();
        finals.insert(a, wei(1100));

        let events = events_with_balances(&ws, &finals);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0], (a, wei(1000), wei(1100)));
    }

    #[test]
    fn test_empty_withdrawals() {
        let events = withdrawal_balance_events(&[], |_| U256::ZERO);
        assert!(events.is_empty());
    }
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
