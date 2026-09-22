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

/// Regression: an `SSTORE` that runs out of gas on its dynamic cost must NOT emit a
/// `StorageChange`. revm writes the `StorageChanged` journal entry before charging dynamic gas,
/// so a naive journal scan would record the would-have-been change with a shifted ordinal even
/// though the opcode halted and the write was reverted.
#[test]
fn storage_sstore_oog() {
    let folder = case_dir("storage_sstore_oog");
    let outcome = run_prestate(&folder).expect("running storage_sstore_oog prestate must succeed");

    let golden = golden_dir(&folder, "block.2713.binpb");
    assert_block_equals_golden(&outcome.block, &golden).expect("captured block must match golden");
}

/// EIP-7843: the mapper must read `slot_number` from the header rather than hardcoding `None`.
#[test]
fn amsterdam_slot_number() {
    let folder = case_dir("amsterdam_slot_number");
    let outcome =
        run_prestate(&folder).expect("running amsterdam_slot_number prestate must succeed");

    let golden = golden_dir(&folder, "block.2099.binpb");
    assert_block_equals_golden(&outcome.block, &golden).expect("captured block must match golden");
}

/// EIP-7928: `run_wrapped_block` (this repo's pipeline/backfill path) has no payload sidecar to
/// source the block access list from, so it must reconstruct it via re-execution and only surface
/// it once the reconstructed hash matches the header's declared `block_access_list_hash`.
#[test]
fn amsterdam_block_access_list() {
    let folder = case_dir("amsterdam_block_access_list");
    let outcome =
        run_prestate(&folder).expect("running amsterdam_block_access_list prestate must succeed");

    let golden = golden_dir(&folder, "block.2099.binpb");
    assert_block_equals_golden(&outcome.block, &golden).expect("captured block must match golden");
}

/// EIP-7928: a header declaring a `block_access_list_hash` that doesn't match what re-execution
/// reconstructs must hard-fail the block rather than silently ship a wrong `block_access_list_rlp`.
#[test]
fn amsterdam_block_access_list_hash_mismatch() {
    let folder = case_dir("amsterdam_block_access_list_hash_mismatch");
    let err = run_prestate(&folder)
        .expect_err("running amsterdam_block_access_list_hash_mismatch prestate must fail");

    let message = err.to_string();
    assert!(
        message.contains("reconstructed block access list hash mismatch"),
        "unexpected error: {message}"
    );
}

fn case_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("cases").join(name)
}

fn golden_dir(case_dir: &PathBuf, name: &str) -> PathBuf {
    case_dir.join(name)
}
