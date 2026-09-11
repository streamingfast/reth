# Changelog

All notable changes to the StreamingFast Firehose fork of reth are documented here.

This changelog covers Firehose-specific changes only. For upstream reth changes, see the
[official reth releases](https://github.com/paradigmxyz/reth/releases).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## reth-v2.5.2-fh3.1

### Changed

- Merge upstream reth v2.5.2 (from v2.5.0). No Firehose code changed.

## reth-v2.5.0-fh3.2

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

### Build

- `install_llvm_ubuntu.sh` configures the apt.llvm.org repository directly instead of
  running that site's `llvm.sh` installer. The installer is fetched unpinned at build time
  and gates on a distro allow-list of its own, so it rejected Debian 13 — the base of the
  `cargo-chef:latest-rust-1.95-trixie` image — even though
  `apt.llvm.org/trixie/llvm-toolchain-trixie-22` carries every package the build needs.

## reth-v2.5.0-fh3.1

Rebase of the Firehose fork onto upstream reth v2.5.0. Covers everything since
`v2.3.0-fh-7`, including the untagged `v2.4.1-fh`, `v2.4.1-fh-1` and
`reth-v2.5.0-fh3.0` builds.

### Added

- Emit the genesis block (block 0) at ExEx startup when the head is still at
  genesis. Genesis is written to the DB without execution, so no tracing hook
  ever fired for it and streams began at block 1. The old block-1 "genesis
  marker" hack is gone: it emitted block 1 through `on_genesis_block` with an
  empty alloc and never traced its transactions. Block 1 now takes the normal
  tracing path.

### Changed

- Rebase onto upstream reth v2.5.0 (from v2.3.0, via v2.4.1).
- Reject `reth_jit` `Enable`/`Unpause` over RPC while the Firehose tracer is
  active, and refuse to start `FirehoseExecutorBuilder::build_evm` when `--jit`
  was passed. JIT-compiled frames only call `log`/`selfdestruct`/`frame_end` on
  the Inspector — `step`/`step_end` never fire — so per-opcode storage and
  gas-reason data silently vanishes under JIT. `--jit` at startup was already
  inert for the Firehose executor builder, but the RPC method is a second,
  independent way to flip JIT on at runtime.

### Fixed

- Collapse the duplicate `alloy-evm` entry left in `Cargo.lock` by the v2.4.1
  merge. With both the unpatched 0.37.1 crate and our `streamingfast/evm` patch
  in the graph, most of the executor pipeline linked against the unpatched copy,
  which does not route system calls through the Inspector — silently dropping
  the block's EIP-4788/EIP-2935 `system_calls` from Firehose output.

### Build

- `Dockerfile.sf`: bump the cargo-chef base image to Rust 1.95 and install LLVM
  in the build stage.
- `sf-release.yml` now builds on `release/*` branch pushes (the fork's branches
  were renamed from `firehose/*`). The sibling release branches
  (`release/optimism-2.x`, `release/base-2.x`, `release/bnb-0.x`) no longer
  publish images or releases — their tags exist only as refs for downstream
  projects to pin.

## v2.3.0-fh-7

### Fixed

- Include the SELFDESTRUCT refund when resolving an account's post-transaction balance. On the truly-destroyed path (EIP-6780: contract created in the same transaction, or pre-Cancun) revm credits the beneficiary in place and records the move only inside its `AccountDestroyed` journal entry — no `BalanceTransfer` is pushed — so the journal walk backing the `RewardTransactionFee` and `GasRefund` events missed it. A coinbase or sender that received a suicide refund then reported an `old_balance` contradicting the `SuicideRefund` event emitted moments earlier. First seen on Ethereum mainnet block 25690108.

## v2.3.0-fh-6

### Fixed

- Emit the value-transfer balance changes when a transaction sends value to a precompile and then fails (e.g. the precompile runs out of gas). The transfer creates a revm `BalanceTransfer` journal entry that is normally read in `call_end`, but a reverted no-step callee has that entry truncated by the checkpoint rollback before the journal walk runs, so both balance changes were dropped. They are now captured at call-enter and re-emitted synthetically on revert, matching geth (which records the transfer that happened before the revert). Aborts that occur *before* the transfer (`OutOfFunds` / `CallTooDeep`) correctly emit nothing.

## v2.3.0-fh-5

### Fixed

- Fix a call/receipt log-count mismatch panic (`assign_ordinal_and_index_to_receipt_logs`: "N call logs but N+1 receipt logs") when a native-precompile log (e.g. a B-20 token event) is emitted at a journal index just freed by a reverted opcode `LOG`. The opcode log advanced the `gather_precompile_logs` watermark via `log_full`; revm truncated it on revert but left the watermark stale-high, so the precompile log was skipped as already-emitted. `gather_precompile_logs` now re-clamps the watermark to the live journal log count, mirroring `gather_precompile_storage_changes`. First seen on Base mainnet block 48387796 (a Uniswap V4 revert-based quote hiding a B-20 log).

## v2.3.0-fh-4

### Added

- Add StreamingFast Docker image build, push and release CI (`Dockerfile.sf`, `.github/workflows/sf-release.yml`). Pushing the `firehose/*` branch or a `*-fh*` tag builds the Firehose-instrumented `reth` and publishes it to `ghcr.io/streamingfast/reth`; tag builds use the `maxperf` profile and attach a `reth_linux_amd64` release asset. The runtime image bundles `fireeth`, which drives `reth` as its reader node.

## v2.3.0-fh-3

### Fixed

- Add a gas-bound cap on `step_keccak256` to prevent an out-of-memory panic for operations that would out-of-gas anyway.

## v2.3.0-fh-2

### Added

- Expose the post-tx balance resolver so chains can supply post-tx balance extras.

## v2.3.0-fh-1

### Added

- Capture native-precompile state changes in the Firehose tracer.

### Changed

- Drive the precompile test through the real `call` / `call_end` hooks.

## v2.3.0-fh

Rebase the Firehose fork onto upstream reth v2.3.0, keeping Firehose tracing intact.

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
