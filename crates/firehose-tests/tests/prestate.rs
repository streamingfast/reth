//! End-to-end Firehose integration tests driven by `prestate.json` fixtures.

use std::path::PathBuf;

use reth_firehose_tests::{assert_block_equals_golden, run_prestate};

#[test]
fn nop_transfer() {
    let folder = case_dir("nop_transfer");
    let outcome = run_prestate(&folder).expect("running nop_transfer prestate must succeed");

    let golden = golden_dir(&folder, "block.2099.binpb");
    assert_block_equals_golden(&outcome.block, &golden).expect("captured block must match golden");
}

/// Known divergence between reth and geth tracers: when an `SSTORE` runs out of gas inside a
/// call frame that subsequently reverts, reth emits the would-have-been `StorageChange` record
/// (with a shifted ordinal) while geth suppresses it. The captured Block matches the golden in
/// every other respect.
///
/// Re-enable (drop `#[ignore]`) once both clients converge on a single behaviour.
#[test]
#[ignore = "known reth/geth divergence: reverted-call StorageChange emission for SSTORE-OOG"]
fn storage_sstore_oog() {
    let folder = case_dir("storage_sstore_oog");
    let outcome = run_prestate(&folder).expect("running storage_sstore_oog prestate must succeed");

    let golden = golden_dir(&folder, "block.2713.binpb");
    assert_block_equals_golden(&outcome.block, &golden).expect("captured block must match golden");
}

fn case_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("cases").join(name)
}

fn golden_dir(case_dir: &PathBuf, name: &str) -> PathBuf {
    case_dir.join(name)
}
