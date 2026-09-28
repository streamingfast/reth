//! The per-transaction internal-call input budget ([`MAX_CALL_INPUT_BYTES_PER_TX_ENV`]).

use crate::inspector::*;

fn call_input_test_tracer() -> firehose_tracer::Tracer {
    firehose_tracer::Tracer::new_with_writer(
        firehose_tracer::config::Config::default(),
        Box::new(Vec::<u8>::new()),
    )
}

#[test]
fn parse_max_call_input_bytes_values() {
    assert_eq!(parse_max_call_input_bytes(None), Ok(None));
    assert_eq!(parse_max_call_input_bytes(Some("")), Ok(None));
    assert_eq!(parse_max_call_input_bytes(Some(" 0 ")), Ok(None));
    assert_eq!(parse_max_call_input_bytes(Some("104857600")), Ok(Some(104_857_600)));
    assert!(parse_max_call_input_bytes(Some("100MiB")).is_err());
    assert!(parse_max_call_input_bytes(Some("-1")).is_err());
}

/// Without a limit (the default) every call input is traced in full.
#[test]
fn call_input_not_capped_without_limit() {
    let mut tracer = call_input_test_tracer();
    let mut insp = FirehoseInspector::new(&mut tracer);
    insp.max_call_input_bytes_per_tx = None;
    let big = vec![0xab; 1 << 20];
    assert_eq!(insp.traced_call_input(0, Address::ZERO, &big).len(), big.len());
    for _ in 0..16 {
        assert_eq!(insp.traced_call_input(1, Address::ZERO, &big).len(), big.len());
    }
}

/// With a limit, internal calls are traced in full while they fit in the per-tx budget,
/// then as their 4-byte selector; the root call is never capped and resets the budget.
#[test]
fn call_input_capped_per_transaction() {
    let mut tracer = call_input_test_tracer();
    let mut insp = FirehoseInspector::new(&mut tracer);
    insp.max_call_input_bytes_per_tx = Some(10);
    let six = [1u8, 2, 3, 4, 5, 6];

    // Root call: never capped, even far above the limit.
    let root = vec![7u8; 100];
    assert_eq!(insp.traced_call_input(0, Address::ZERO, &root), &root[..]);

    // 6 bytes fit (6/10), the next 6 do not: selector only.
    assert_eq!(insp.traced_call_input(1, Address::ZERO, &six), &six[..]);
    assert_eq!(insp.traced_call_input(2, Address::ZERO, &six), &six[..4]);
    // A later small input that still fits the remaining budget is traced in full.
    assert_eq!(insp.traced_call_input(1, Address::ZERO, &[9, 9, 9]), &[9, 9, 9][..]);
    // Inputs shorter than a selector stay as they are when capped.
    assert_eq!(insp.traced_call_input(1, Address::ZERO, &six), &six[..4]);
    assert_eq!(insp.traced_call_input(1, Address::ZERO, &[1, 2]), &[1, 2][..]);

    // 3 calls capped (6-4, 6-4 and 2-2 bytes left out = 4 bytes dropped).
    assert_eq!(insp.tx_call_inputs_capped, 3);
    assert_eq!(insp.tx_call_input_bytes_dropped, 4);
    // Traced in full: 6 + 3 (charged to the budget); selectors kept: 4 + 4 + 2.
    assert_eq!(insp.tx_call_input_bytes, 9);
    assert_eq!(insp.tx_call_input_bytes_kept_capped, 10);

    // The next transaction's root call starts a fresh budget.
    assert_eq!(insp.traced_call_input(0, Address::ZERO, &root), &root[..]);
    assert_eq!(insp.tx_call_inputs_capped, 0);
    assert_eq!(insp.traced_call_input(1, Address::ZERO, &six), &six[..]);
}

/// A root CREATE also starts a fresh budget (contract-creation transactions have no
/// depth-0 `call`).
#[test]
fn call_input_budget_resets_on_root_create() {
    let mut tracer = call_input_test_tracer();
    let mut insp = FirehoseInspector::new(&mut tracer);
    insp.max_call_input_bytes_per_tx = Some(10);
    let eight = [0u8; 8];
    assert_eq!(insp.traced_call_input(1, Address::ZERO, &eight).len(), 8);
    assert_eq!(insp.traced_call_input(1, Address::ZERO, &eight).len(), 4);
    insp.reset_tx_call_input_budget();
    assert_eq!(insp.traced_call_input(1, Address::ZERO, &eight).len(), 8);
}
