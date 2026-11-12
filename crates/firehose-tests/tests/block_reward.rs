//! Regression test: staged sync emits the pre-Merge block and uncle rewards.
//!
//! The block executor credits the rewards in its finish, outside the EVM, so the inspector never
//! sees them. The wrapper must emit one `RewardMineUncle` change per ommer, then the miner's
//! `RewardMineBlock` change, in the order go-ethereum's ethash `accumulateRewards` applies them.
//!
//! It lives in its own integration-test binary because it installs the process-wide tracer.

use std::sync::Arc;

use alloy_consensus::Header;
use alloy_primitives::{Address, U256};
use evm2::evm::{AccountInfo, InMemoryDB};
use firehose_tracer::pb::sf::ethereum::r#type::v2::{balance_change::Reason, BigInt};
use reth_chainspec::{Chain, ChainSpec};
use reth_ethereum_primitives::{Block, BlockBody};
use reth_evm::execute::Executor;
use reth_evm_ethereum::EthEvmConfig;
use reth_firehose::FirehoseBlockExecutor;
use reth_firehose_tests::parse_fire_block_for;
use reth_primitives_traits::RecoveredBlock;

const ETHER: u128 = 1_000_000_000_000_000_000;
const MINER: Address = Address::with_last_byte(1);
const UNCLE: Address = Address::with_last_byte(2);

#[test]
fn staged_sync_emits_uncle_rewards_then_the_block_reward() {
    let buffer = reth_firehose::init_tracer_with_buffer(Chain::mainnet().id(), None, None, None);

    // Petersburg without the Merge: a 2 ETH base reward.
    let chain_spec = ChainSpec::builder()
        .chain(Chain::mainnet())
        .genesis(Default::default())
        .petersburg_activated()
        .build();
    let evm_config = EthEvmConfig::new(Arc::new(chain_spec));

    let miner_start = U256::from(7u64);
    let mut db = InMemoryDB::default();
    db.insert_account_info(&MINER, AccountInfo { balance: miner_start, ..Default::default() });

    // The second ommer pays the miner, so the miner gets two changes.
    let ommer = |beneficiary, number| Header { beneficiary, number, ..Default::default() };
    let block = RecoveredBlock::new_unhashed(
        Block {
            header: Header {
                number: 100,
                beneficiary: MINER,
                gas_limit: 30_000_000,
                ..Default::default()
            },
            body: BlockBody {
                transactions: Vec::new(),
                ommers: vec![ommer(UNCLE, 99), ommer(MINER, 98)],
                withdrawals: None,
            },
        },
        Vec::new(),
    );

    let mut executor = FirehoseBlockExecutor::new(evm_config, db);
    executor.execute_and_trace_one(&block).expect("execute_and_trace_one succeeds");
    // Reaching `into_state` confirms the last block passed validation, which flushes it.
    let state = executor.into_state();
    let fire_block = parse_fire_block_for(&buffer.get_bytes(), 100).expect("the block is emitted");

    let uncle = U256::from(7 * 2 * ETHER / 8); // (99 + 8 - 100) * 2 / 8
    let miner_uncle = U256::from(6 * 2 * ETHER / 8); // (98 + 8 - 100) * 2 / 8
    let miner_block = U256::from(2 * ETHER + 2 * (2 * ETHER / 32)); // 2 + 2 * 2 / 32
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
            (UNCLE, U256::ZERO, uncle, Reason::RewardMineUncle),
            (MINER, miner_start, miner_start + miner_uncle, Reason::RewardMineUncle),
            (
                MINER,
                miner_start + miner_uncle,
                miner_start + miner_uncle + miner_block,
                Reason::RewardMineBlock
            ),
        ]
    );
    let ordinals =
        fire_block.balance_changes.iter().map(|change| change.ordinal).collect::<Vec<_>>();
    assert!(ordinals.is_sorted(), "the changes are out of order: {ordinals:?}");

    // The emitted values end where the executor left the balances.
    let final_balance = |address| state.account(&address).unwrap().info.as_ref().unwrap().balance;
    assert_eq!(final_balance(UNCLE), uncle);
    assert_eq!(final_balance(MINER), miner_start + miner_uncle + miner_block);
}

/// Decodes a Firehose `BigInt`, where an absent value is zero.
fn value(value: Option<&BigInt>) -> U256 {
    value.map(|value| U256::from_be_slice(&value.bytes)).unwrap_or_default()
}
