## v1.11.4-fh-2

### Added

- Flashblocks support in `reth-firehose`: a dedicated per-flashblock tracer can now emit pre-canonical partial block events with `FlashBlockData` annotations alongside the canonical block tracer.
- `FirehoseBlockTracer` gains `start_flashblock_local` and `mark_flashblock` methods for emitting flashblock events without going through the global tracer lock.
- `SynchronizedStdout`, `STDOUT_LOCK`, `init_stdout_lock()`, and `stdout_lock()` added to `reth-firehose` so the global tracer and a concurrent flashblock tracer share the same `Arc<Mutex<()>>` before each stdout `write_all`, preventing interleaved output when both tracers are active.
- `reth-firehose-tests` now exposes `Prestate`, `TraceContext`, `seed_cache_db`, `build_account_info`, `parse_fire_block_for`, `decode_hex`, and the serde deserializer helpers (`deser_u64_str`, `deser_opt_u128_str`, `deser_opt_u256_str`) as public for reuse in downstream test crates (e.g. base-firehose-tests).

### Changed

- `init_tracer` now accepts `firehose_tracer::config::Config` directly instead of a pre-built `Tracer`. It internally calls `init_stdout_lock()`, wraps the result in `SynchronizedStdout`, and constructs the `Tracer` via `new_with_writer`, ensuring the lock and tracer are initialised atomically and consistently.
- Upgraded `evm-firehose-tracer-rs` to the latest version.

## v1.11.4-fh-1

- Initial release
