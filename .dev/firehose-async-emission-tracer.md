# Plan: Async Emission in `evm-firehose-tracer-rs`

## Goal

Decouple the heavy work (protobuf encoding, base64 encoding, stdout write) from the
execution thread by introducing a configurable `EmissionMode`. The current synchronous
blocking path must remain fully functional as `EmissionMode::Blocking` (the default
should remain backward-compatible). A background writer thread handles the async path.

---

## 1. Add `EmissionMode` to `Config`

```rust
/// Controls when and how encoded blocks are written to stdout.
#[derive(Debug, Clone)]
pub enum EmissionMode {
    /// Current behaviour: encode → base64 → write, all inline on the calling thread.
    Blocking,

    /// Encode and write in a dedicated background thread.
    /// The execution thread enqueues a raw block and returns immediately.
    /// Backpressure is applied when the channel reaches `channel_capacity`.
    Async {
        channel_capacity: usize, // sensible default: 32
    },

    /// Switch automatically based on block age relative to wall-clock time.
    ///
    /// Blocks older than `live_threshold` use the Async path (pipeline / catch-up sync).
    /// Blocks within `live_threshold` of now use the Blocking path (live tip, low latency).
    Auto {
        channel_capacity: usize,
        live_threshold: std::time::Duration, // sensible default: 60s
    },
}

impl Default for EmissionMode {
    fn default() -> Self {
        Self::Blocking
    }
}
```

Add `emission_mode: EmissionMode` to the existing `Config` struct. The field should
default to `EmissionMode::Blocking` so existing callers are unaffected.

---

## 2. Cursor file

The background writer (and the blocking path) must record the last block number
successfully written to stdout so that callers can detect gaps after unclean
shutdowns.

```rust
// New field on Config
pub cursor_path: Option<PathBuf>, // None → cursor file disabled
```

After each successful stdout flush the writer atomically writes the block number
to `cursor_path` (write to `<path>.tmp`, then rename — avoids torn reads).

Format: a single decimal integer followed by `\n`, e.g. `"21000042\n"`.

Expose on `Tracer`:

```rust
impl Tracer {
    /// Returns the block number last confirmed written to stdout, read from the
    /// cursor file if `cursor_path` is set. Returns `None` if no cursor exists yet.
    pub fn last_confirmed_block(&self) -> Option<u64>;
}
```

---

## 3. Background writer thread

When mode is `Async` or `Auto`, `Tracer::new()` spawns a single OS thread
(not a tokio task — stdout write is blocking I/O, keep it off the async runtime).

```
Tracer owns:
  sender: Option<SyncSender<EncodedBlock>>
  writer_thread: Option<JoinHandle<()>>
```

`EncodedBlock` is an internal struct holding the already-collected raw block data
ready for protobuf serialisation. **Protobuf encoding and base64 encoding also move
into the writer thread** — the sending side only clones/moves the collected fields.

Writer thread loop:
```
loop {
    match receiver.recv() {
        Ok(raw) => {
            let proto = encode_protobuf(raw);
            let b64   = base64_encode(proto);
            write_fire_line_to_stdout(b64);
            update_cursor_file(raw.block_num);
        }
        Err(_) => break, // sender dropped → drain complete → exit
    }
}
```

---

## 4. `on_block_end` changes

```rust
fn on_block_end(&mut self, ...) {
    match effective_mode(&self.config, block_timestamp) {
        EmissionMode::Blocking => {
            // existing path: encode + write inline
            let proto = encode_protobuf(&self.current_block);
            let b64   = base64_encode(proto);
            write_fire_line_to_stdout(b64);
            update_cursor_file(block_num);
        }
        EmissionMode::Async { .. } | EmissionMode::Auto { .. } => {
            let raw = std::mem::take(&mut self.current_block);
            // send() blocks only if channel is full (backpressure)
            self.sender.as_ref().unwrap().send(raw).expect("writer thread died");
        }
    }
}
```

`effective_mode` for `Auto`: compare `block_timestamp` to `SystemTime::now()`;
if `now - block_timestamp <= live_threshold` return `Blocking`, else return
`Async`.

---

## 5. Shutdown / drain

Add a `ShutdownHandle` type:

```rust
pub struct ShutdownHandle {
    // dropping this drops the sender, which signals the writer thread to drain
    _sender: SyncSender<EncodedBlock>,
    thread:  Option<JoinHandle<()>>,
}

impl ShutdownHandle {
    /// Block until the writer thread has flushed all queued blocks and exited.
    pub fn drain(mut self) {
        drop(self._sender); // signal EOF to writer
        if let Some(t) = self.thread.take() { t.join().ok(); }
    }
}
```

`Tracer` exposes:

```rust
impl Tracer {
    /// Returns a handle that, when dropped or `drain()`-ed, waits for the
    /// background writer to flush all pending blocks.
    /// Returns `None` when mode is `Blocking` (no background thread).
    pub fn shutdown_handle(&self) -> Option<ShutdownHandle>;
}
```

`Tracer` itself also calls `drain` in its `Drop` impl as a safety net (so if the
caller forgets, we still flush). The `Drop` impl should log a warning if the thread
had un-flushed items on unexpected exit.

---

## 6. `Blocking` path cursor update

For consistency, the Blocking path must also update the cursor file after each
stdout write. This is already described in §4 above.

---

## 7. Things explicitly out of scope for this plan

- Re-trace of gap blocks on startup — that is reth-side logic (see reth integration plan).
- Any changes to `reth/crates/firehose/` beyond bumping the dependency version.
- Changing the `FIRE BLOCK` line format.

---

## 8. Acceptance criteria

- [ ] `EmissionMode::Blocking` behaviour is byte-for-byte identical to current behaviour (existing integration tests pass unchanged).
- [ ] `EmissionMode::Async` emits all blocks in order with no drops under normal shutdown.
- [ ] `EmissionMode::Auto` uses Blocking for blocks within `live_threshold`, Async for older blocks.
- [ ] Cursor file is written after every block in all modes when `cursor_path` is set.
- [ ] `last_confirmed_block()` returns correct value after restart with cursor file present.
- [ ] `ShutdownHandle::drain()` waits for the writer thread to flush all queued blocks before returning.
- [ ] Channel backpressure prevents unbounded memory growth when writer is slower than executor.
