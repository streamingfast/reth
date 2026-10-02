# Changelog

All notable changes to the StreamingFast Firehose fork of bnb-chain/reth are documented here.

This changelog covers Firehose-specific changes only. For upstream changes, see the
[bnb-chain/reth releases](https://github.com/bnb-chain/reth/releases).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## Unreleased

### Changed

- `reth` no longer installs a Firehose execution extension (ExEx). It only emitted `FIRE INIT`
  and the genesis block; blocks are traced by the engine. Both are now emitted at startup before
  the consensus engine exists, so no block can be traced ahead of `FIRE INIT`, and the node
  stops writing the ExEx write-ahead log (and its `WAL contains too many blocks` warning).

### Removed

- `reth_firehose::run_exex` and `reth_firehose::emit_genesis_block_if_empty`. Call
  `reth_firehose::init_blockchain(&provider)` from the node builder's `on_component_initialized`
  hook instead. With `FIREHOSE_DISABLED` set it emits nothing. `reth-firehose` no longer depends
  on `reth-exex`.

## bnb-v0.1.3-fh3.1-1

### Fixed

- `FirehoseEvmConfig` now forwards `evm_with_env`, `evm_with_env_and_inspector` and
  `create_block_builder` to the wrapped config. BSC applies its block-override fork rules in
  `evm_with_env`, so `eth_call`, `eth_estimateGas`, `debug_traceCall` and `eth_simulateV1` with
  a time or number override that crosses a hardfork ran under the original block's fork.
- `ChainTracingConfig::is_deferred_system_tx` now receives the transaction's effective gas price
  at the block's base fee instead of its fee cap. A BSC system transaction sent as a dynamic-fee
  transaction with a zero tip and a non-zero fee cap was traced as a normal transaction, which
  panicked the node when the BSC executor deferred it without pushing a receipt.

## bnb-v0.1.3-fh3.1

Rebased onto bnb-chain/reth `v0.1.3` (`6e08308b`), the reth release that bnb-chain/reth-bsc
`v0.1.3` (Chapel Jenner hardfork) depends on. That upstream range covers RPC fixes (state
overrides reaching chain execution metadata, a block-env override hook, buffered `trace_filter`
replays, a blocking-task deadlock), pruner fixes and a header history segment, blob store
garbage collection, an optional minimal storage mode, a RocksDB `max_open_files` setting, and
an engine fix that registers the pending changeset entry inside the background task. It touches
no Firehose file, so the instrumentation is carried across unchanged.

### Added

- `reth_firehose::precompile_effects` lets a stateful precompile report each storage write,
  code install and log as it makes it. The inspector then emits them in that order when the
  precompile call ends, as geth's hooked state does: storage changes and logs keep their
  relative order, a slot written twice in one call reports its intermediate value, and a call
  that reverts keeps its writes and logs. Precompiles that do not report keep the previous
  behavior, which reads their writes back from the journal when the call ends. BSC's CAS20
  precompiles use it.

## bnb-v0.1.2-fh3.1-2

### Changed

- Firehose `Call.keccak_preimages` now keeps only the preimages that explain a storage slot written by the transaction: the hash is the storage key, the storage key is the hash plus a small offset (a struct field or array position), or the hash appears inside another kept preimage (up to 16 levels). Preimages of hashes that never lead to a written storage slot, and preimages over 256 bytes, are no longer recorded (see streamingfast/reth#57).
- Firehose limits the call input and return data a transaction records: past 50 MiB of internal call input or 25 MiB of return data, later calls keep only the 4-byte selector of their input and no return data, and set `Call.input_truncated` / `Call.return_data_truncated`. The root call is never cut, and a block that would still encode to more than 1 GiB has both limits halved until it fits (see streamingfast/reth#57).

## bnb-v0.1.2-fh3.1

Rebased onto bnb-chain/reth `v0.1.2` (`f66079c2`). That upstream range is RPC-only —
`eth_getLogs` topic-position selection, rejection of log filters naming the pending block,
`totalDifficulty` on header responses, tracer null-config handling,
`getTransactionDataAndReceipt` null result, the new `--rpc.batchrequestlimit` flag, and an h2
bump — plus CI changes. It touches no Firehose file, so the instrumentation is carried across
unchanged.

## bnb-v0.1.1-fh3.2

### Fixed

- Stop advertising a finalized block that is not an ancestor of the block being emitted. Every
  `FIRE BLOCK` line carried the node's finalized head as of the moment the block executed, so a
  block from a side branch was published with a LIB number naming the canonical chain's block at
  that height; the consumer marked its own block at that height irreversible and then saw the
  reorg replace it. The advertised block is now the node's finalized head when that head is on
  the emitted block's own chain, the fork point where its branch left the canonical chain
  otherwise, and genesis when the branch cannot be tied to the canonical chain at all. Seen on
  BSC mainnet at block 120653740, where a four-block side branch was published with LIB
  120653741 — one of those blocks naming a height above its own number.

## Unreleased

This fork is consumed as a library by
[streamingfast/reth-bsc](https://github.com/streamingfast/reth-bsc), which builds the
Firehose-instrumented `reth-bsc` binary and Docker image; no binaries or images are
released from this repository.

### Fixed

- Include the SELFDESTRUCT refund when resolving an account's post-transaction balance. On
  the truly-destroyed path (EIP-6780: contract created in the same transaction, or
  pre-Cancun) revm credits the beneficiary in place and records the move only inside its
  `AccountDestroyed` journal entry — no `BalanceTransfer` is pushed — so the journal walk
  backing the `RewardTransactionFee` and `GasRefund` events missed it. A coinbase or sender
  that received a suicide refund then reported an `old_balance` contradicting the
  `SuicideRefund` event emitted moments earlier. Ported from streamingfast/reth
  `v2.3.0-fh-7`.

### Added

- Initial Firehose instrumentation on top of bnb-chain/reth `v0.0.10`, ported from
  streamingfast/reth `firehose/2.x` (`v2.3.0-fh-6`). Includes the `reth-firehose` crate
  (inspector, block tracer, executor wrappers, ExEx), live engine-tree tracing on both the
  standard and triedb validation paths, and pipeline (staged sync) tracing.

### Changed

- Adapted to this fork's dependency set (reth 2.2 base): revm 38 and alloy-evm 0.34, with
  `alloy-evm` patched to `streamingfast/evm` branch `sf/v0.34.0` so system calls
  (EIP-4788, EIP-2935, ...) are routed through the inspector.
- EIP-4788 / EIP-2935 system calls are reported with `gas_limit` 30000000 (the EIP-specified
  system-call gas limit, matching geth), instead of the 31566720 reported by the revm 40 based
  `firehose/2.x` fork. Golden test blocks were re-blessed accordingly.
