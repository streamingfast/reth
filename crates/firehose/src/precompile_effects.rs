//! Live record of the state changes a stateful precompile makes.
//!
//! A native precompile writes storage, code and logs straight on the journal, so no opcode
//! hook fires. Gathering those writes from the journal when the call ends loses three things
//! geth's tracer keeps, because geth's hooked state reports each write as it happens:
//!
//! - the order of storage changes, code changes and logs relative to each other;
//! - the intermediate value of a slot written more than once in the same call;
//! - every write of a call that reverts, since revm truncates the journal before the inspector's
//!   `call_end` runs.
//!
//! A precompile that calls [`record_storage_change`], [`record_code_change`] and [`record_log`]
//! at each write gets geth's behavior: the inspector emits the recorded effects, in order,
//! when the precompile call ends, and skips the journal gathering for that call. Precompiles
//! that record nothing keep the journal gathering.
//!
//! Recording is only on while a [`crate::inspector::FirehoseInspector`] is inside a
//! transaction on this thread and tracing is not suspended, so untraced execution pays a
//! thread-local read per write and records nothing.

use alloy_primitives::{Address, Bytes, Log, B256, U256};
use std::cell::{Cell, RefCell};

/// One state change made by a precompile, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrecompileEffect {
    Storage { address: Address, key: U256, old_value: U256, new_value: U256 },
    Code { address: Address, old_hash: B256, old_code: Bytes, new_hash: B256, new_code: Bytes },
    Log(Log),
}

thread_local! {
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    static EFFECTS: RefCell<Vec<PrecompileEffect>> = const { RefCell::new(Vec::new()) };
}

/// Returns `true` when a write made now would be recorded. Lets a precompile skip the extra
/// reads it needs only for recording (such as the code being replaced).
pub fn is_recording() -> bool {
    RECORDING.with(|r| r.get()) && !crate::chain_tracing::is_tracing_suspended()
}

/// Records a storage write. Writes that leave the slot unchanged are ignored, as geth does.
pub fn record_storage_change(address: Address, key: U256, old_value: U256, new_value: U256) {
    if old_value != new_value {
        push(PrecompileEffect::Storage { address, key, old_value, new_value });
    }
}

/// Records a code install on `address`.
pub fn record_code_change(
    address: Address,
    old_hash: B256,
    old_code: Bytes,
    new_hash: B256,
    new_code: Bytes,
) {
    push(PrecompileEffect::Code { address, old_hash, old_code, new_hash, new_code });
}

/// Records a log the precompile emits.
pub fn record_log(log: &Log) {
    push(PrecompileEffect::Log(log.clone()));
}

fn push(effect: PrecompileEffect) {
    if is_recording() {
        EFFECTS.with(|e| e.borrow_mut().push(effect));
    }
}

/// Turns recording on and drops anything left from an earlier transaction.
pub(crate) fn start_transaction() {
    EFFECTS.with(|e| e.borrow_mut().clear());
    RECORDING.with(|r| r.set(true));
}

/// Turns recording off and drops anything not yet taken.
pub(crate) fn end_transaction() {
    RECORDING.with(|r| r.set(false));
    EFFECTS.with(|e| e.borrow_mut().clear());
}

/// Takes the effects recorded since the last take.
pub(crate) fn take() -> Vec<PrecompileEffect> {
    EFFECTS.with(|e| std::mem::take(&mut *e.borrow_mut()))
}
