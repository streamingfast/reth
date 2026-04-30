# Plan: Reth Integration — Async Firehose Emission

Prerequisite: `evm-firehose-tracer-rs` plan fully implemented and published.

---

## 1. Bump dependency

Update `Cargo.toml` to the new `firehose-tracer` version that includes
`EmissionMode`, `ShutdownHandle`, and `last_confirmed_block()`.

---

## 2. Expose `EmissionMode` via CLI / config

In `bin/reth/src/main.rs` (or wherever `firehose_tracer::Config` is built),
wire a CLI flag or env var to `Config::emission_mode`:

```
--firehose.emission-mode  blocking | async | auto   (default: auto)
--firehose.channel-capacity  <n>                    (default: 32)
--firehose.live-threshold    <secs>                 (default: 60)
--firehose.cursor-path       <path>                 (default: <datadir>/firehose.cursor)
```

Keep it minimal — env-var override is fine if CLI flags feel heavy.

---

## 3. Shutdown wiring

In the node startup code (where `init_tracer(...)` is called), capture the
shutdown handle and register it with reth's shutdown lifecycle:

```rust
let tracer = firehose_tracer::Tracer::new(config);
let shutdown_handle = tracer.shutdown_handle(); // Option<ShutdownHandle>
init_tracer(tracer);

// Wire drain into node shutdown — one line
if let Some(handle) = shutdown_handle {
    node.on_shutdown(move || handle.drain());
}
```

The exact `node.on_shutdown` API depends on reth's node builder; adapt as needed.
The invariant: `ShutdownHandle::drain()` must be called **before** the process exits,
and **after** the execution stage has stopped producing blocks.

---

## 4. Gap detection and re-trace on startup

Before the sync pipeline starts, check for a gap between the cursor and the
execution stage checkpoint:

```rust
// In the Firehose startup hook (before pipeline launch)
let last_written = reth_firehose::tracer().last_confirmed_block();
let exec_tip     = provider.get_stage_checkpoint(StageId::Execution)?.block_number;

if let Some(last) = last_written {
    if last < exec_tip {
        firehose_re_trace_range(last + 1..=exec_tip, &provider).await?;
    }
}
```

`firehose_re_trace_range` lives in `reth/crates/firehose/src/runner.rs` (or a new
`re_trace.rs`). It:

1. Iterates block numbers in the range.
2. For each block, fetches the sealed block + pre-execution state from the provider
   (same approach as `debug_traceBlock`).
3. Runs `FirehoseWrappedExecutor` with the Firehose inspector in emit-only mode —
   **no DB writes**.
4. Calls `on_block_end()` which emits the `FIRE BLOCK` line (and updates the cursor).

**Pruning guard**: before starting the re-trace loop, verify that
`provider.earliest_full_block()` (or equivalent) is ≤ `last + 1`. If the required
historical state has been pruned, log a fatal error with a clear message explaining
that Firehose requires an archive node (or that pruning must be configured to retain
state back to `last_written`). Do not silently skip blocks.

---

## 5. Acceptance criteria

- [ ] `--firehose.emission-mode auto` is the default; `blocking` matches previous behaviour exactly.
- [ ] `ShutdownHandle::drain()` is called on graceful shutdown before process exit.
- [ ] On startup with a gap, `firehose_re_trace_range` re-emits the missing blocks before sync resumes.
- [ ] If pruned state prevents re-trace, the node exits with a clear fatal error.
- [ ] No changes to the `FIRE BLOCK` line format or tracer event semantics.
