//! Setup guard: fails with a clear message when system calls stop reaching the inspector.
//!
//! evm2 runs system calls without the inspector unless the executor opts in with
//! `set_inspect_system_calls(true)`. When that opt-in is lost, the Firehose block has no system
//! calls and every later ordinal shifts, which a golden diff shows only as an opaque mismatch.

use std::path::PathBuf;

use reth_firehose_tests::run_prestate;

/// `nop_transfer` is a Cancun block whose header carries a `parentBeaconBlockRoot`, so it runs the
/// EIP-4788 beacon root system call.
#[test]
fn cancun_block_emits_beacon_root_system_call() {
    let case = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases/nop_transfer");
    let outcome = run_prestate(&case).expect("running the nop_transfer prestate");

    assert!(
        !outcome.block.system_calls.is_empty(),
        "the Firehose block of a Cancun block has no system calls, so the EIP-4788 beacon root \
         call did not reach the inspector; check that FirehoseWrappedExecutor calls \
         set_inspect_system_calls(true)",
    );
}
