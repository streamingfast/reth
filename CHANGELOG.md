# Changelog

All notable changes to the StreamingFast Firehose fork of reth are documented here.

This changelog covers Firehose-specific changes only. For upstream reth changes, see the
[official reth releases](https://github.com/paradigmxyz/reth/releases).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## arc-v2.2.0-fh3.1-2

### Fixed

- `FirehoseEvmConfig` now delegates `builder_for_next_block` to the wrapped config instead of
  inheriting the trait default. Arc overrides that method to take the block gas limit from its
  on-chain `ProtocolConfig`; behind the wrapper the default ran instead, the payload builder used
  reth's generic gas-limit target (36M, clamped to `parent + parent/1024 - 1`) and then rejected
  its own block with "block gas limit 30029295 does not match expected 30000000". Every node
  running the wrapper failed to build blocks, independent of whether the tracer was enabled.

## arc-v2.2.0-fh3.1-1

### Fixed

- Sweep the journal for logs and storage changes at `create_end`, as `call_end` already does.
  Init code cannot emit logs without a `LOG` opcode on Ethereum, but Arc's SELFDESTRUCT journals
  an EIP-7708 `Transfer` log directly, so a contract whose init code self-destructs (Arc mainnet
  block 2,426,896) produced a receipt with one more log than the call trace and the tracer aborted
  with "mismatch between call logs and receipt logs".

## arc-v2.2.0-fh3.1

Arc-specific release line (`release/arc-2.x`) for [arc-node](https://github.com/circlefin/arc-node),
which pins upstream reth v2.2.0. Branched from the last v2.2.0-based commit of `release/reth-2.x`
(`45190921f`) and back-ports the Firehose fixes that landed on later release lines.

### Added

- `ChainSemantics` (`reth_firehose::ChainSemantics`, `init_tracer_with_semantics`,
  `init_tracer_with_buffer_and_semantics`, `chain_semantics()`): lets a chain declare that the
  EIP-1559 base fee is credited to the block beneficiary instead of burned. The wrapped executor
  then reports the whole effective gas price as the beneficiary's `RewardTransactionFee` instead
  of only the priority fee. Default is unchanged (mainnet burn); Arc sets `base_fee_burned: false`.

### Back-ported (from `release/reth-2.x`, in order)

- Capture native-precompile state changes in the Firehose tracer (`dc97689c1`, `83df090c3`).
- Expose the post-tx balance resolver for chain post-tx extras (`1b93c7022`).
- Gas-bound cap on `step_keccak256` to prevent an OOM panic (`630bffdd5`).
- Fix missing block finality on the live engine path (`5e055f0f0`): the payload validator now
  passes the node's finalized head to the block tracer.
- Emit reverted value-transfer balance changes for precompile calls (`c951b27d6`) and re-clamp the
  precompile log watermark after a reverted opcode log (`262ae6bbd`).
- Credit the SELFDESTRUCT refund in the post-tx balance resolver (`16736229b`).
- Emit the genesis block (block 0) on empty-chain start and drop the block-1 "genesis marker"
  hack (`2df3964d6`).

### Not back-ported

- JIT-vs-Firehose guards, the v2.4.x `state_hook` drift fix and the `alloy-evm` lock dedupe: the
  corresponding upstream code does not exist in reth v2.2.0.

## v2.2.0-fh

First Firehose-instrumented release on top of upstream reth v2.2.0.

### Added

- Add flashblocks support to `reth-firehose`: `start_flashblock_local` and `mark_flashblock` methods on `FirehoseBlockTracer` allow partial block ("flashblock") boundaries to be emitted during block execution.
- Add `SynchronizedStdout` for coordinated stdout writes across multiple concurrent tracer instances; stdout lock initialization is now handled internally by `init_tracer`.
- Expose prestate types and helpers as `pub` in `reth-firehose-tests` crate to allow reuse in downstream integration test suites.

### Changed

- `init_tracer` now accepts `Config` directly and sets up the stdout lock internally, removing the need for callers to manage stdout coordination themselves.
- Update `firehose-tracer` dependency to version 5.1.1.

### Fixed

- Restore the Firehose live-path hooks in the engine-tree payload validator so blocks arriving through the engine API (`newPayload` / `forkchoiceUpdated`) are traced again. The hooks had been dropped during a merge, leaving only the historical/stage execution path instrumented.
