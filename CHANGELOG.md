# Changelog

All notable changes to the StreamingFast Firehose fork of reth are documented here.

This changelog covers Firehose-specific changes only. For upstream reth changes, see the
[official reth releases](https://github.com/paradigmxyz/reth/releases).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## v2.2.0-fh

First Firehose-instrumented release on top of upstream reth v2.2.0.

### Added

- Add flashblocks support to `reth-firehose`: `start_flashblock_local` and `mark_flashblock` methods on `FirehoseBlockTracer` allow partial block ("flashblock") boundaries to be emitted during block execution.
- Add `SynchronizedStdout` for coordinated stdout writes across multiple concurrent tracer instances; stdout lock initialization is now handled internally by `init_tracer`.
- Expose prestate types and helpers as `pub` in `reth-firehose-tests` crate to allow reuse in downstream integration test suites.

### Changed

- `init_tracer` now accepts `Config` directly and sets up the stdout lock internally, removing the need for callers to manage stdout coordination themselves.
- Update `firehose-tracer` dependency to version 5.1.1.
