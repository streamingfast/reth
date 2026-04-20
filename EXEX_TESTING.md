# ExEx Unit Testing Guide

## Infrastructure

Reth ships `reth-exex-test-utils` with everything needed. Key types:
- `test_exex_context_with_chain_spec` / `test_exex_context` — spins up a full in-memory node
- `TestExExHandle` — send notifications, assert events
- `PollOnce` — drive an ExEx future one step at a time

## Setup

```rust
use reth_exex_test_utils::{test_exex_context_with_chain_spec, PollOnce};
use reth_execution_types::{Chain, ExecutionOutcome};
use reth_chainspec::{ChainSpecBuilder, MAINNET};
use reth_ethereum_primitives::{Block, BlockBody};
use alloy_consensus::Header;
use alloy_eips::eip4895::{Withdrawal, Withdrawals};
```

## Chain spec with withdrawals (Shanghai+)

Withdrawals were introduced at Shanghai. Use `shanghai_activated()` or higher:

```rust
let chain_spec = Arc::new(
    ChainSpecBuilder::default()
        .chain(MAINNET.chain)
        .genesis(MAINNET.genesis.clone())
        .shanghai_activated()
        .build(),
);
let (ctx, handle) = test_exex_context_with_chain_spec(chain_spec).await?;
```

Available hardfork builders (in order):
`paris_activated`, `shanghai_activated`, `cancun_activated`, `prague_activated`, `osaka_activated`

## Building blocks with withdrawals

```rust
let block = Block {
    header: Header {
        parent_hash: handle.genesis.hash(),
        number: 1,
        withdrawals_root: Some(/* alloy_trie computed root, or B256::ZERO for logic-only tests */),
        ..Default::default()
    },
    body: BlockBody {
        transactions: vec![],
        withdrawals: Some(Withdrawals::new(vec![
            Withdrawal {
                index: 0,
                validator_index: 42,
                address: some_address,
                amount: 1_000_000_000, // in Gwei
            },
        ])),
        ..Default::default()
    },
}
.try_into_recovered()?;
```

## Option A: Full execution (correct state, receipts)

Pattern from `crates/exex/exex/src/backfill/test_utils.rs`:

```rust
use reth_evm_ethereum::EthEvmConfig;
use reth_provider::LatestStateProviderRef;
use reth_revm::database::StateProviderDatabase;

let provider = handle.provider_factory.provider()?;
let block_output = EthEvmConfig::ethereum(chain_spec.clone())
    .batch_executor(StateProviderDatabase::new(LatestStateProviderRef::new(&provider)))
    .execute(&block)?;

let execution_outcome = ExecutionOutcome {
    bundle: block_output.state.clone(),
    receipts: vec![block_output.receipts.clone()],
    first_block: block.number(),
    requests: vec![block_output.requests.clone()],
};

let chain = Chain::from_block(block, execution_outcome, None);
```

For multi-block chains use `execute_batch(vec![&block1, &block2])` which returns a single
`ExecutionOutcome` spanning all blocks — matches what a real `ChainCommitted` carries.

Remember to commit blocks to the DB too if your ExEx re-executes them later:

```rust
let provider_rw = handle.provider_factory.provider_rw()?;
provider_rw.append_blocks_with_state(vec![block.clone()], &execution_outcome, Default::default())?;
provider_rw.commit()?;
```

## Option B: Logic-only (no execution, faster)

For tests that only read block fields (e.g. withdrawals list) and don't re-execute:

```rust
let chain = Chain::from_block(block, ExecutionOutcome::default(), None);
```

`withdrawals_root` in the header can be `Default::default()` here — consensus validation
is not run in ExEx unit tests.

## Sending the notification and driving the ExEx

```rust
// Send ChainCommitted
handle.send_notification_chain_committed(chain).await?;

// Drive the ExEx one poll — should process the notification and return Pending
std::pin::pin!(my_exex(ctx)).poll_once().await?;

// Assert FinishedHeight was emitted
handle.assert_event_finished_height(expected_num_hash)?;

// Assert no unexpected extra events
handle.assert_events_empty();
```

`poll_once()` returns:
- `Ok(())` — future returned `Pending` (correct, ExEx is still running)
- `Err(FutureIsReady)` — ExEx resolved unexpectedly
- `Err(FutureError(e))` — ExEx returned an error

## Re-execution bug notes

When re-executing blocks received in a `ChainCommitted` notification:

1. **Use `batch_executor` across the whole chain** — seed it with
   `history_by_block_hash(first_block.parent_hash())`, then call `execute_one` for each block
   in order. Never create a new `State` per block: intermediate blocks' parent state is not
   yet in the historical DB.

2. **Use `history_by_block_hash` not `state_by_block_hash`** — the `state_by_` variant
   also checks pending state, which can cause confusion during initial sync.

3. **Emit `FinishedHeight` only after re-execution completes** — premature emission tells
   Reth it is safe to prune state you still need.

## Key source references

| What | Where |
|------|-------|
| Test context / handle | `crates/exex/test-utils/src/lib.rs` |
| Block + execution test helpers | `crates/exex/exex/src/backfill/test_utils.rs` |
| `Chain` type | `crates/evm/execution-types/src/chain.rs` |
| `ExExNotification` types | `crates/exex/types/src/notification.rs` |
| `re-execute` command (batch_executor pattern) | `crates/cli/commands/src/re_execute.rs` |
| Hello-world ExEx example | `examples/exex-hello-world/src/main.rs` |
| Subscription ExEx example | `examples/exex-subscription/src/main.rs` |
