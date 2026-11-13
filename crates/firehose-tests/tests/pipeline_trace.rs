//! Regression test: the pipeline path emits the same Firehose block as the golden.
//!
//! The prestate goldens come from `run_wrapped_block` with a local tracer. Staged sync instead
//! runs [`Executor::execute_and_trace_one`] with the process-wide tracer and flushes the block
//! only in `into_state` or on the next block. This test runs a prestate block through that path
//! and checks the emitted block against the same golden.
//!
//! It lives in its own integration-test binary because it installs the process-wide tracer.

use std::path::PathBuf;

use reth_evm::execute::Executor;
use reth_evm_ethereum::EthEvmConfig;
use reth_firehose::FirehoseBlockExecutor;
use reth_firehose_tests::{
    assert_block_equals_golden, load_prestate, parse_fire_block_for, LoadedPrestate,
};

#[test]
fn pipeline_executor_matches_the_golden() {
    let case = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases/nop_transfer");
    let LoadedPrestate { prestate, chain_spec, block, db } =
        load_prestate(&case).expect("loading the nop_transfer prestate");

    let config = &prestate.genesis.config;
    let buffer = reth_firehose::init_tracer_with_buffer(
        config.chain_id,
        config.shanghai_time,
        config.cancun_time,
        config.prague_time,
    );
    let mut executor = FirehoseBlockExecutor::new(EthEvmConfig::new(chain_spec), db);
    executor.execute_and_trace_one(&block).expect("execute_and_trace_one succeeds");
    // Reaching `into_state` confirms the last block passed validation, which flushes it.
    let _ = executor.into_state();

    let fire_block = parse_fire_block_for(&buffer.get_bytes(), block.header().number)
        .expect("the traced block is emitted");
    assert_block_equals_golden(&fire_block, &case.join("block.2099.binpb"))
        .expect("the pipeline block matches the golden");
}
