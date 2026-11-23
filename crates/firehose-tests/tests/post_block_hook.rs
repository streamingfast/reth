//! Regression test: a chain's post-block hook reaches the traced block, through the hooks of the
//! EVM configuration that [`FirehoseEvmConfig`] wraps.
//!
//! A chain whose block executor changes state in `finish`, outside the EVM, reports those changes
//! with [`PostTxExtras::capture_post_block`]. Both finish variants must capture before the inner
//! finish and emit after the withdrawal balance changes: the pipeline must install the wrapped
//! configuration's hooks and finish with `finish_with_block_access_list`, the live engine path
//! finishes with `finish_with_prepared_block_access_list`.
//!
//! It lives in its own integration-test binary because it installs the process-wide tracer.

use std::{any::TypeId, cell::Cell, ops::DerefMut, path::PathBuf};

use alloy_eips::eip4895::Withdrawal;
use alloy_primitives::{Address, U256};
use firehose_tracer::{
    config::{ChainConfig, Config},
    pb::sf::ethereum::r#type::v2::{balance_change::Reason, BigInt, Block as FirehoseBlock},
    Tracer,
};
use firehose_tracer_test::BlockInvariants;
use reth_ethereum_primitives::{Block, EthPrimitives};
use reth_evm::{
    execute::{BlockExecutor, Executor},
    ConfigureEvm, EvmEnv, EvmEnvFor, ExecutionCtxFor,
};
use reth_evm_ethereum::EthEvmConfig;
use reth_firehose::{
    BlockFees, FirehoseBlockTracer, FirehoseEvmConfig, FirehoseInspector, FirehoseLiveHooks,
    FirehoseWrappedExecutor, NoPreTxAdjust, PostBlockChanges, PostTxExtras,
};
use reth_firehose_tests::{load_prestate, parse_fire_block_for, LoadedPrestate};
use reth_primitives_traits::{HeaderTy, RecoveredBlock, SealedBlock, SealedHeader};
use reth_trie_common::bundle_state::BundleState;

/// Receives the block's only withdrawal.
const RECIPIENT: Address = Address::with_last_byte(0x77);
/// Gets a change from [`TestHooks::emit_post_tx_extras`] after each transaction.
const POST_TX_MARKER: Address = Address::with_last_byte(0x88);
/// Gets the post-block change, which spans [`RECIPIENT`]'s balance across the inner finish.
const POST_BLOCK_MARKER: Address = Address::with_last_byte(0x99);
/// A reason the block produces nowhere else.
const POST_BLOCK_REASON: Reason = Reason::DaoRefundContract;

thread_local! {
    /// The `(block number, coinbase, chain id, spec)` [`TestHooks::capture_post_block`] received
    /// on this test's thread.
    static CAPTURED_BLOCK: Cell<Option<(u64, Address, u64, evm2::SpecId)>> =
        const { Cell::new(None) };
}

#[test]
fn post_block_changes_follow_the_withdrawals_on_the_pipeline() {
    // `FirehoseEvmConfig` selects the wrapped configuration's hooks for the engine path.
    assert_eq!(
        TypeId::of::<<FirehoseEvmConfig<TestEvmConfig> as FirehoseLiveHooks>::PostTxExtras>(),
        TypeId::of::<TestHooks>()
    );

    let case = load_case();
    let config = &case.prestate.genesis.config;
    let buffer = reth_firehose::init_tracer_with_buffer(
        config.chain_id,
        config.shanghai_time,
        config.cancun_time,
        config.prague_time,
    );
    let evm_config = FirehoseEvmConfig::new(TestEvmConfig(EthEvmConfig::new(case.chain_spec)));
    let mut executor = evm_config.batch_executor(case.db);
    executor.execute_and_trace_one(&case.block).expect("execute_and_trace_one succeeds");
    // Reaching `into_state` confirms the last block passed validation, which flushes it.
    let _ = executor.into_state();
    let fire_block = parse_fire_block_for(&buffer.get_bytes(), case.block.header().number)
        .expect("block emitted");

    assert_post_block_changes(&evm_config.inner, &case.block, config.chain_id, &fire_block);
}

#[test]
fn post_block_changes_follow_the_withdrawals_on_the_live_path() {
    let case = load_case();
    let config = &case.prestate.genesis.config;
    let (tracer, buffer) = Tracer::with_buffer(
        Config::default(),
        ChainConfig {
            chain_id: config.chain_id,
            shanghai_time: config.shanghai_time,
            cancun_time: config.cancun_time,
            prague_time: config.prague_time,
            verkle_time: None,
        },
        "reth-firehose-tests",
        env!("CARGO_PKG_VERSION"),
    );
    let evm_config = TestEvmConfig(EthEvmConfig::new(case.chain_spec));
    let block = &case.block;
    let tracer = FirehoseBlockTracer::start_local::<EthPrimitives>(
        Box::new(tracer),
        block.sealed_block(),
        None,
    );

    let evm_env = evm_config.evm_env(block.header()).expect("evm env of the block");
    let fees = BlockFees::new(&evm_env);
    let inner =
        evm_config.executor_for_block(case.db, block.sealed_block()).expect("block executor");
    let mut executor = FirehoseWrappedExecutor::with_hooks(
        inner,
        tracer.inspector(),
        fees,
        block.body().withdrawals.clone(),
        NoPreTxAdjust,
        TestHooks,
    );
    executor.apply_pre_execution_changes().expect("pre-execution changes apply");
    for transaction in block.transactions_recovered() {
        executor.execute_transaction(transaction).expect("transaction executes");
    }
    executor.finish_with_prepared_block_access_list().expect("block finishes");
    tracer.mark_verified();
    let fire_block =
        parse_fire_block_for(&buffer.get_bytes(), block.header().number).expect("block emitted");

    assert_post_block_changes(&evm_config, block, config.chain_id, &fire_block);
}

/// The `nop_transfer` case, with one withdrawal to [`RECIPIENT`] added to its block.
fn load_case() -> LoadedPrestate {
    let case = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases/nop_transfer");
    let mut loaded = load_prestate(&case).expect("loading the nop_transfer prestate");
    let (mut block, senders) = loaded.block.split();
    let withdrawal = Withdrawal { index: 0, validator_index: 0, address: RECIPIENT, amount: 1 };
    block.body.withdrawals = Some(vec![withdrawal].into());
    loaded.block = RecoveredBlock::new_unhashed(block, senders);
    loaded
}

/// Checks that [`TestHooks`] ran on `fire_block`, traced from `block`, and that its post-block
/// change comes after the withdrawal's.
fn assert_post_block_changes(
    evm_config: &TestEvmConfig,
    block: &RecoveredBlock<Block>,
    chain_id: u64,
    fire_block: &FirehoseBlock,
) {
    BlockInvariants::assert(fire_block).expect("the block holds the invariants");

    // The hook got the block it runs for.
    let evm_env = evm_config.evm_env(block.header()).expect("evm env of the block");
    assert_eq!(
        CAPTURED_BLOCK.get(),
        Some((block.header().number, block.header().beneficiary, chain_id, evm_env.spec_id()))
    );

    // The wrapper installs the post-transaction hook.
    let transaction = &fire_block.transaction_traces[0];
    assert!(
        transaction.calls[0]
            .balance_changes
            .iter()
            .any(|change| change.address == POST_TX_MARKER.as_slice()),
        "the post-transaction hook did not run"
    );

    // The hook read the balance before the inner finish and got the final state after it, and
    // its change comes after the withdrawal's.
    let gwei = U256::from(1_000_000_000u64);
    let changes = fire_block
        .balance_changes
        .iter()
        .map(|change| {
            (
                Address::from_slice(&change.address),
                value(change.old_value.as_ref()),
                value(change.new_value.as_ref()),
                change.reason(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        changes,
        vec![
            (RECIPIENT, U256::ZERO, gwei, Reason::Withdrawal),
            (POST_BLOCK_MARKER, U256::ZERO, gwei, POST_BLOCK_REASON),
        ]
    );
    assert!(fire_block.balance_changes[0].ordinal < fire_block.balance_changes[1].ordinal);
}

/// Decodes a Firehose `BigInt`, where an absent value is zero.
fn value(value: Option<&BigInt>) -> U256 {
    value.map(|value| U256::from_be_slice(&value.bytes)).unwrap_or_default()
}

/// Chain hooks that mark each transaction and report [`RECIPIENT`]'s balance across the inner
/// finish as a change of [`POST_BLOCK_MARKER`].
#[derive(Debug, Default, Clone, Copy)]
struct TestHooks;

impl<E: reth_evm::Evm> PostTxExtras<E> for TestHooks {
    fn emit_post_tx_extras<G: DerefMut<Target = firehose_tracer::Tracer>>(
        &self,
        _evm: &mut E,
        inspector: &FirehoseInspector<G>,
        _gas_used: u64,
        _base_fee: u64,
    ) {
        inspector.tracer_mut().on_balance_change(
            POST_TX_MARKER,
            U256::ZERO,
            U256::from(1u64),
            Reason::RewardTransactionFee,
        );
    }

    fn capture_post_block(
        &self,
        evm: &mut E,
        block: &BlockFees,
    ) -> Option<Box<dyn PostBlockChanges>> {
        CAPTURED_BLOCK.set(Some((
            block.block_number(),
            block.coinbase(),
            block.chain_id(),
            block.spec(),
        )));
        let balance = evm
            .account_info(&RECIPIENT)
            .expect("reading the recipient")
            .map(|info| info.balance)
            .unwrap_or_default();
        Some(Box::new(RecipientChange { balance }))
    }
}

/// [`RECIPIENT`]'s balance before the inner finish.
#[derive(Debug)]
struct RecipientChange {
    balance: U256,
}

impl PostBlockChanges for RecipientChange {
    fn emit(&self, tracer: &mut firehose_tracer::Tracer, state: &BundleState) {
        let balance = state
            .account(&RECIPIENT)
            .and_then(|account| account.info.as_ref())
            .map(|info| info.balance)
            .unwrap_or_default();
        tracer.on_balance_change(POST_BLOCK_MARKER, self.balance, balance, POST_BLOCK_REASON);
    }
}

/// An EVM configuration with its own Firehose hooks, standing in for a chain's.
#[derive(Debug, Clone)]
struct TestEvmConfig(EthEvmConfig);

impl FirehoseLiveHooks for TestEvmConfig {
    type PreTxAdjust = NoPreTxAdjust;
    type PostTxExtras = TestHooks;
}

impl ConfigureEvm for TestEvmConfig {
    type Primitives = EthPrimitives;
    type Error = <EthEvmConfig as ConfigureEvm>::Error;
    type NextBlockEnvCtx = <EthEvmConfig as ConfigureEvm>::NextBlockEnvCtx;
    type BlockExecutorFactory = <EthEvmConfig as ConfigureEvm>::BlockExecutorFactory;
    type BlockAssembler = <EthEvmConfig as ConfigureEvm>::BlockAssembler;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        self.0.block_executor_factory()
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        self.0.block_assembler()
    }

    fn evm_env(&self, header: &HeaderTy<EthPrimitives>) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.0.evm_env(header)
    }

    fn next_evm_env(
        &self,
        parent: &HeaderTy<EthPrimitives>,
        attributes: &Self::NextBlockEnvCtx,
    ) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.0.next_evm_env(parent, attributes)
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<reth_ethereum_primitives::Block>,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error>
    where
        Self: 'a,
    {
        self.0.context_for_block(block)
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader<HeaderTy<EthPrimitives>>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<ExecutionCtxFor<'_, Self>, Self::Error> {
        self.0.context_for_next_block(parent, attributes)
    }
}
