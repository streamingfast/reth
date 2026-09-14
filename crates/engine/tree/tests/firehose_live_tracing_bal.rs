//! Regression test for block access list (EIP-7928) validation on the Firehose live-block path.
//!
//! When the Firehose tracer is active, `engine_newPayload` executes blocks through
//! `execute_and_trace_block` instead of `execute_block`. That path must build the block access
//! list during execution so post-execution validation compares it against the header's
//! `block_access_list_hash`. Without it, the comparison is skipped and a Firehose node accepts
//! (and streams) a block that every other node rejects.
//!
//! It lives in its own integration-test binary because it installs a process-wide tracer.

use alloy_primitives::{Address, Bytes, B256};
use alloy_rpc_types_engine::{
    ExecutionData, ExecutionPayload, PayloadAttributes, PayloadStatusEnum,
};
use eyre::Result;
use reth_chainspec::{ChainSpecBuilder, MAINNET};
use reth_e2e_test_utils::setup_engine;
use reth_engine_tree::tree::TreeConfig;
use reth_ethereum_primitives::TransactionSigned;
use reth_node_ethereum::EthereumNode;
use reth_primitives_traits::Block as _;
use std::sync::Arc;

#[tokio::test]
async fn live_payload_validation_checks_block_access_list() -> Result<()> {
    reth_tracing::init_test_tracing();

    // The tracer must be installed before the node validates any block so the live path routes
    // execution through `execute_and_trace_block`.
    let buffer =
        reth_firehose::init_tracer_with_buffer(MAINNET.chain.id(), Some(0), Some(0), Some(0));

    let chain_spec = Arc::new(
        ChainSpecBuilder::default()
            .chain(MAINNET.chain)
            .genesis(
                serde_json::from_str(include_str!(
                    "../../../e2e-test-utils/src/testsuite/assets/genesis.json"
                ))
                .unwrap(),
            )
            .amsterdam_activated()
            .build(),
    );

    let (mut nodes, _wallet) = setup_engine::<EthereumNode>(
        1,
        chain_spec,
        false,
        TreeConfig::default().with_has_enough_parallelism(true),
        amsterdam_payload_attributes,
    )
    .await?;
    let mut node = nodes.pop().unwrap();

    // Build block 1 locally. Even without transactions its access list is not empty: the
    // EIP-4788 and EIP-2935 system calls write storage.
    let built = node.new_payload().await?;
    let valid: ExecutionData = built.into();
    let valid_hash = valid.payload.block_hash();

    // Replace the access list with an empty one and recompute the block hash, so the payload is
    // self-consistent but its access list no longer matches what execution produces.
    let mut tampered = valid.clone();
    let ExecutionPayload::V4(payload) = &mut tampered.payload else {
        eyre::bail!("expected an Amsterdam (V4) execution payload");
    };
    payload.block_access_list = Bytes::from_static(&[0xc0]);
    let tampered_hash = tampered
        .payload
        .clone()
        .try_into_block_with_sidecar::<TransactionSigned>(&tampered.sidecar)?
        .seal_slow()
        .hash();
    set_block_hash(&mut tampered.payload, tampered_hash);
    assert_ne!(tampered_hash, valid_hash);

    let engine = &node.inner.add_ons_handle.beacon_engine_handle;

    let status = engine.new_payload(tampered).await?;
    match &status.status {
        PayloadStatusEnum::Invalid { validation_error } => assert!(
            validation_error.contains("block access list hash mismatch"),
            "unexpected validation error: {validation_error}"
        ),
        other => panic!("payload with a mismatched block access list was not rejected: {other:?}"),
    }

    let status = engine.new_payload(valid).await?;
    assert_eq!(status.status, PayloadStatusEnum::Valid, "untampered payload must be valid");

    // Only the valid block may reach Firehose consumers.
    let text = String::from_utf8(buffer.get_bytes()).expect("captured tracer output is UTF-8");
    let fire_block_1: Vec<&str> =
        text.lines().filter(|line| line.starts_with("FIRE BLOCK 1 ")).collect();
    assert_eq!(fire_block_1.len(), 1, "expected exactly one FIRE BLOCK line for block #1:\n{text}");
    assert!(
        fire_block_1[0].contains(&format!("{valid_hash:x}")),
        "FIRE BLOCK line is not for the valid block {valid_hash}:\n{text}"
    );
    assert!(
        !text.contains(&format!("{tampered_hash:x}")),
        "the rejected block was emitted to Firehose:\n{text}"
    );

    Ok(())
}

const fn amsterdam_payload_attributes(timestamp: u64) -> PayloadAttributes {
    PayloadAttributes {
        timestamp,
        prev_randao: B256::ZERO,
        suggested_fee_recipient: Address::ZERO,
        withdrawals: Some(vec![]),
        parent_beacon_block_root: Some(B256::ZERO),
        slot_number: Some(timestamp),
        target_gas_limit: None,
    }
}

const fn set_block_hash(payload: &mut ExecutionPayload, hash: B256) {
    match payload {
        ExecutionPayload::V1(p) => p.block_hash = hash,
        ExecutionPayload::V2(p) => p.payload_inner.block_hash = hash,
        ExecutionPayload::V3(p) => p.payload_inner.payload_inner.block_hash = hash,
        ExecutionPayload::V4(p) => p.payload_inner.payload_inner.payload_inner.block_hash = hash,
    }
}
