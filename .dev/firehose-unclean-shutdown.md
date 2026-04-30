# Plan: Unclean Shutdown Handling (Research Spike)

This is a scoping document for future deeper investigation. The gap-detection +
re-trace mechanism (see reth integration plan) covers the **recovery** side.
This plan covers what happens *during* an unclean shutdown and what hardening
is still needed.

---

## Problem statement

An unclean shutdown (kill -9, OOM kill, power loss, panic) can leave the system
in a state where:

- Some blocks are executed and committed to the DB (execution stage checkpoint = N).
- The background writer thread has partially drained its channel.
- The cursor file records the last successfully *written* block M where M ≤ N.
- The Firehose consumer on the other end of stdout has received blocks up to some
  K where K ≤ M (stdout is buffered at the OS level; the OS buffer may not have
  been flushed).

The worst case is K < M < N. On restart, re-trace covers M+1..N, but blocks
K+1..M that *were* written to the OS buffer but never received by the consumer
are lost.

---

## Questions to investigate

1. **OS stdout buffering**: When the reth process dies, does the kernel flush the
   pipe buffer to the reader? In practice on Linux, data already written to the
   kernel pipe buffer *is* delivered to the reader even after the writer dies.
   Confirm this under OOM-kill and SIGKILL scenarios. If true, K == M and the gap
   reduces to M+1..N only.

2. **Cursor write atomicity**: The cursor is written after `write()` returns. If
   the process dies between the `write()` and the cursor update, the cursor
   underestimates M. The reader has the block but re-trace will re-emit it.
   Firehose consumers must be **idempotent to duplicate blocks at the seam**.
   Confirm this is already guaranteed by the Firehose protocol (block numbers are
   monotonic, consumer deduplicates by number).

3. **Cursor file placement**: Should the cursor live on the same filesystem as the
   DB? On a different one? If on the same FS and a disk failure causes both DB and
   cursor to be inconsistent simultaneously, recovery becomes harder.

4. **Signal handling**: SIGTERM is caught by reth today — does the shutdown sequence
   guarantee the execution stage stops *before* `ShutdownHandle::drain()` is called?
   If execution is still pushing blocks into the channel during drain, the drain may
   never complete. Needs a clear ordering invariant.

5. **Panic recovery**: A panic in the background writer thread will cause
   `sender.send()` to return `Err` on the execution thread. What should happen?
   Options: (a) propagate as a fatal error, (b) fall back to blocking mode for
   remaining blocks, (c) ignore and lose the block. Option (a) is safest.

---

## Potential mitigations to evaluate

- **`fsync` the cursor file** after each write (expensive but guarantees durability).
  Benchmark the overhead on a fast NVMe before committing.
- **Two-phase cursor**: write a "pending" cursor before stdout write, "confirmed"
  cursor after. On restart, if "pending" != "confirmed", the block is ambiguous —
  re-trace it (idempotent as noted above).
- **Dedicated pipe flush**: call `stdout().flush()` explicitly after each block in
  the Blocking path (the Async path writer already owns stdout and can flush after
  each write).

---

## Out of scope for this spike

- Changes to the Firehose consumer protocol.
- Multi-process / multi-writer scenarios.
- Crash-consistency of the reth DB itself (that is reth's problem, not Firehose's).

---

## Output expected from the spike

A short decision document (≤ 1 page) answering questions 1–5 above and recommending
which mitigations (if any) are worth implementing, with rough effort estimates.
