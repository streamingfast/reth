# Changelog

All notable changes to the StreamingFast Firehose fork of reth are documented here.

This changelog covers Firehose-specific changes only. For upstream reth changes, see the
[official reth releases](https://github.com/paradigmxyz/reth/releases).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## v2.3.0-fh-4

### Added

- Add StreamingFast Docker image build, push and release CI (`Dockerfile.sf`, `.github/workflows/sf-release.yml`). Pushing the `firehose/*` branch or a `*-fh*` tag builds the Firehose-instrumented `reth` and publishes it to `ghcr.io/streamingfast/reth`; tag builds use the `maxperf` profile and attach a `reth_linux_amd64` release asset. The runtime image bundles `fireeth`, which drives `reth` as its reader node.

### Fixed

- Emit the value-transfer balance changes when a transaction sends value to a precompile and then fails (e.g. the precompile runs out of gas). The transfer creates a revm `BalanceTransfer` journal entry that is normally read in `call_end`, but a reverted no-step callee has that entry truncated by the checkpoint rollback before the journal walk runs, so both balance changes were dropped. They are now captured at call-enter and re-emitted synthetically on revert, matching geth (which records the transfer that happened before the revert). Aborts that occur *before* the transfer (`OutOfFunds` / `CallTooDeep`) correctly emit nothing.

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
