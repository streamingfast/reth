use alloy_primitives::{Address, Bytes, Log as AlloyLog, B256, U256};
use firehose_tracer::{
    pb,
    types::{Opcode, StringError},
};
use reth_revm::revm::{
    context_interface::{ContextTr, JournalTr},
    inspector::{Inspector, JournalExt},
    interpreter::{
        interpreter::EthInterpreter, interpreter_types::Jumps, CallInputs, CallOutcome,
        CreateInputs, CreateOutcome, Interpreter,
    },
    primitives::KECCAK_EMPTY,
};
use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
};

struct StepContext {
    start_journal_idx: usize,
    opcode: u8,
}

/// FirehoseInspector captures execution traces for the Firehose format
/// It hooks into EVM execution via the Inspector trait to build a complete call tree
pub struct FirehoseInspector<'a> {
    tracer: &'a mut firehose_tracer::Tracer,

    /// The last opcode executed in `step`, used to detect SSTORE for storage change tracking in
    /// `step_end`.
    last_step: Option<StepContext>,

    /// Index into the journal up to which balance/nonce/code changes have already been processed.
    /// Advances at each call/create entry and exit to avoid processing the same entries twice.
    journal_processed_up_to: usize,

    /// Tracks the last balance emitted for each address within the current transaction.
    /// Used to determine the delta for post-execution balance changes (gas refund, miner fee)
    /// that happen after all inspector call hooks but before commit.
    balance_tracker: HashMap<Address, U256>,

    /// When true, the next `step` call should process journal changes to pick up
    /// the value transfer BalanceTransfer entry pushed by frame_init AFTER the
    /// call/create hook returned. This ensures value transfers for ALL calls
    /// (successful or failed, root or nested) are captured before any revert.
    pending_value_transfer_check: bool,

    /// Addresses that executed SELFDESTRUCT and were truly destroyed (AccountDestroyed
    /// journal entry) during the current transaction.
    selfdestruct_addresses: HashSet<Address>,

    /// Captured nonce/code state for self-destructed accounts, to be emitted after post-tx
    /// balance changes (gas refund, reward). This matches Geth 1.17.x's Finalise timing
    /// where nonce resets and code clears happen after gas accounting.
    pending_selfdestruct_cleanups: Vec<SelfdestructCleanupEntry>,

    // increments at the end of each transaction, to get proper block index for logs
    log_block_index: u32,

    // last seen number of logs in a given transaction
    trx_logs_count: u32,
}

impl<'a> Debug for FirehoseInspector<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirehoseInspector")
            .field("last_step", &self.last_step.as_ref().map(|s| s.opcode))
            .field("journal_processed_up_to", &self.journal_processed_up_to)
            .field("balance_tracker_keys", &self.balance_tracker.keys())
            .field("pending_value_transfer_check", &self.pending_value_transfer_check)
            .field("selfdestruct_addresses", &self.selfdestruct_addresses)
            .finish()
    }
}

/// Pre-destruction state for a self-destructed account, captured at root call exit.
struct SelfdestructCleanupEntry {
    address: Address,
    nonce: u64,
    code_hash: B256,
    code: Bytes,
}

impl<'a> FirehoseInspector<'a> {
    /// Create a new FirehoseInspector with a mutable reference to the tracer.
    pub fn new(tracer: &'a mut firehose_tracer::Tracer) -> Self {
        Self {
            tracer,
            last_step: None,
            journal_processed_up_to: 0,
            balance_tracker: HashMap::new(),
            pending_value_transfer_check: false,
            selfdestruct_addresses: HashSet::new(),
            pending_selfdestruct_cleanups: Vec::new(),
            log_block_index: 0,
            trx_logs_count: 0,
        }
    }

    /// Returns a mutable reference to the tracer, allowing the runner to call tracer lifecycle
    /// methods (on_tx_start, on_tx_end, etc.) while the inspector owns the tracer borrow.
    pub fn tracer_mut(&mut self) -> &mut firehose_tracer::Tracer {
        self.tracer
    }

    /// Capture KECCAK256 preimage from the interpreter state.
    ///
    /// Called from `step` before the opcode executes. The stack still holds
    /// the inputs: stack[0] = offset, stack[1] = size.
    ///
    /// Since `step` fires before memory resize, the memory region may not yet
    /// be allocated. Like Geth's `scope.Memory.GetPtr`, we zero-pad any bytes
    /// beyond current memory length to produce a complete preimage.
    fn step_keccak256(
        tracer: &mut firehose_tracer::Tracer,
        interp: &mut Interpreter<EthInterpreter>,
    ) {
        let (Ok(offset), Ok(size)) = (interp.stack.peek(0), interp.stack.peek(1)) else {
            return;
        };

        let len = size.saturating_to::<usize>();
        if len == 0 {
            tracer.on_keccak_preimage(alloy_primitives::utils::KECCAK256_EMPTY, &[]);
            return;
        }

        let offset = offset.saturating_to::<usize>();
        let mem_len = interp.memory.len();

        if offset.checked_add(len).is_some_and(|end| end <= mem_len) {
            // Happy path: entire region is within current memory, no allocation
            let preimage = interp.memory.slice_len(offset, len);
            let hash = alloy_primitives::keccak256(&*preimage);
            tracer.on_keccak_preimage(hash, &preimage);
        } else {
            // Memory not yet resized (step fires before resize_memory!).
            // Zero-pad like Geth's Memory.GetPtr to produce a complete preimage.
            let mut buf = vec![0u8; len];
            if offset < mem_len {
                let copy_len = (mem_len - offset).min(len);
                buf[..copy_len].copy_from_slice(&interp.memory.slice_len(offset, copy_len));
            }
            let hash = alloy_primitives::keccak256(&buf);
            tracer.on_keccak_preimage(hash, &buf);
        }
    }

    /// Process new balance/nonce/code journal entries since the last scan and emit the
    /// corresponding tracer events. Called at strategic points:
    ///  - BEFORE on_call_enter in `call`/`create` hooks (so parent entries stay on parent)
    ///  - BEFORE on_call_exit in `call_end`/`create_end` hooks (so child entries stay on child)
    fn process_journal_changes<CTX>(&mut self, context: &mut CTX)
    where
        CTX: ContextTr,
        CTX::Journal: JournalExt,
    {
        use reth_revm::revm::context::JournalEntry;

        let journal_len = context.journal().journal().len();

        // Handle journal rollback: when a call reverts, revm truncates journal entries
        // from that call's execution. If our pointer is past the new end, snap it to the
        // current length so we don't re-process parent entries that were already emitted.
        if self.journal_processed_up_to > journal_len {
            self.journal_processed_up_to = journal_len;
        }

        if self.journal_processed_up_to == journal_len {
            return;
        }

        // Collect the entries we need to process (clone to avoid borrow conflicts when reading
        // state)
        let entries: Vec<_> = context.journal().journal()
            [self.journal_processed_up_to..journal_len]
            .iter()
            .cloned()
            .collect();
        self.journal_processed_up_to = journal_len;

        let reason = pb::sf::ethereum::r#type::v2::balance_change::Reason::Transfer;

        for entry in entries {
            match entry {
                JournalEntry::BalanceChange { address, old_balance } => {
                    let new_balance = context
                        .journal()
                        .evm_state()
                        .get(&address)
                        .map(|a| a.info.balance)
                        .unwrap_or(U256::ZERO);
                    if old_balance != new_balance {
                        self.tracer.on_balance_change(address, old_balance, new_balance, reason);
                        self.balance_tracker.insert(address, new_balance);
                    }
                }
                JournalEntry::BalanceTransfer { from, to, balance } => {
                    if !balance.is_zero() {
                        let evm_state = context.journal().evm_state();
                        let new_from =
                            evm_state.get(&from).map(|a| a.info.balance).unwrap_or(U256::ZERO);
                        let old_from = new_from.saturating_add(balance);
                        let new_to =
                            evm_state.get(&to).map(|a| a.info.balance).unwrap_or(U256::ZERO);
                        let old_to = new_to.saturating_sub(balance);

                        self.tracer.on_balance_change(from, old_from, new_from, reason);
                        self.balance_tracker.insert(from, new_from);
                        self.tracer.on_balance_change(to, old_to, new_to, reason);
                        self.balance_tracker.insert(to, new_to);
                    }
                }
                JournalEntry::NonceChange { address, previous_nonce } => {
                    let new_nonce = context
                        .journal()
                        .evm_state()
                        .get(&address)
                        .map(|a| a.info.nonce)
                        .unwrap_or(0);
                    self.tracer.on_nonce_change(address, previous_nonce, new_nonce);
                }
                JournalEntry::NonceBump { address } => {
                    // NonceBump is pushed by EIP-7702 delegate() and by CREATE frame
                    // setup (bump_nonce for the caller). EIP-7702 entries are already
                    // handled by process_eip7702_auth_list and skipped via
                    // journal_processed_up_to advancement.
                    let new_nonce = context
                        .journal()
                        .evm_state()
                        .get(&address)
                        .map(|a| a.info.nonce)
                        .unwrap_or(0);
                    let old_nonce = new_nonce.saturating_sub(1);
                    self.tracer.on_nonce_change(address, old_nonce, new_nonce);
                }
                JournalEntry::CodeChange { address } => {
                    let account = context.journal().evm_state().get(&address);
                    if let Some(account) = account {
                        let new_hash = account.info.code_hash;
                        let new_code = account
                            .info
                            .code
                            .as_ref()
                            .map(|b| b.original_bytes())
                            .unwrap_or_default();
                        // CodeChange is always from empty code to new code (revert restores to
                        // KECCAK_EMPTY)
                        self.tracer.on_code_change(
                            address,
                            KECCAK_EMPTY,
                            new_hash,
                            &[],
                            new_code.as_ref(),
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// Process SELFDESTRUCT balance changes from journal entries pushed during the opcode.
    ///
    /// SELFDESTRUCT causes revm to push journal entries (AccountDestroyed or BalanceTransfer)
    /// with balance mutations. We emit these as suicide-specific balance change reasons
    /// (SuicideWithdraw/SuicideRefund) and advance journal_processed_up_to so that
    /// process_journal_changes doesn't re-emit them with the wrong reason.
    ///
    /// For the post-Cancun case where contract == target and the contract was NOT created
    /// locally, revm pushes no journal entry and doesn't change state. We still emit the
    /// withdraw/refund balance changes to match Geth's behavior (net-zero change).
    fn process_selfdestruct_balance_changes<CTX>(&mut self, context: &mut CTX, start_idx: usize)
    where
        CTX: ContextTr,
        CTX::Journal: JournalExt,
    {
        use pb::sf::ethereum::r#type::v2::balance_change::Reason;
        use reth_revm::revm::context::JournalEntry;

        let journal = context.journal().journal();
        let new_entries = &journal[start_idx..];

        // Look for the selfdestruct-related journal entry
        let mut found = false;
        for entry in new_entries {
            match entry {
                JournalEntry::AccountDestroyed { address, target, had_balance, .. } => {
                    // Only emit balance changes when there's actually balance to move.
                    // When had_balance == 0, there's nothing to withdraw or refund.
                    if !had_balance.is_zero() {
                        // Contract's balance was zeroed
                        self.tracer.on_balance_change(
                            *address,
                            *had_balance,
                            U256::ZERO,
                            Reason::SuicideWithdraw,
                        );
                        self.balance_tracker.insert(*address, U256::ZERO);

                        if address != target {
                            // Target received the balance (already mutated by revm)
                            let target_balance = context
                                .journal()
                                .evm_state()
                                .get(target)
                                .map(|a| a.info.balance)
                                .unwrap_or(U256::ZERO);
                            let old_target = target_balance.saturating_sub(*had_balance);
                            self.tracer.on_balance_change(
                                *target,
                                old_target,
                                target_balance,
                                Reason::SuicideRefund,
                            );
                            self.balance_tracker.insert(*target, target_balance);
                        }
                        // Self-beneficiary locally created (address == target): only the
                        // initial WITHDRAW is emitted. Geth 1.17.x does not emit the
                        // REFUND+WITHDRAW round-trip that older versions produced.
                    }

                    self.selfdestruct_addresses.insert(*address);
                    found = true;
                    break;
                }
                JournalEntry::BalanceTransfer { from, to, balance } => {
                    // Post-Cancun, non-locally-created, address != target:
                    // revm pushes BalanceTransfer instead of AccountDestroyed.
                    // Only emit when there's actual balance to transfer.
                    if !balance.is_zero() {
                        let from_balance = context
                            .journal()
                            .evm_state()
                            .get(from)
                            .map(|a| a.info.balance)
                            .unwrap_or(U256::ZERO);
                        self.tracer.on_balance_change(
                            *from,
                            from_balance.saturating_add(*balance),
                            from_balance,
                            Reason::SuicideWithdraw,
                        );
                        self.balance_tracker.insert(*from, from_balance);

                        let to_balance = context
                            .journal()
                            .evm_state()
                            .get(to)
                            .map(|a| a.info.balance)
                            .unwrap_or(U256::ZERO);
                        self.tracer.on_balance_change(
                            *to,
                            to_balance.saturating_sub(*balance),
                            to_balance,
                            Reason::SuicideRefund,
                        );
                        self.balance_tracker.insert(*to, to_balance);
                    }

                    found = true;
                    break;
                }
                _ => {}
            }
        }

        if !found {
            // Post-Cancun, not locally created, address == target: no journal entry pushed,
            // no state change. Geth 1.17.x does not emit any balance changes for this case
            // because the net effect is zero (withdraw + refund to self cancel out).
            return;
        }

        // Advance past the selfdestruct journal entries so process_journal_changes
        // doesn't re-emit them with wrong reasons.
        self.journal_processed_up_to = context.journal().journal().len();
    }

    /// Process EIP-7702 auth list delegations that occur during pre-execution.
    ///
    /// `apply_eip7702_auth_list` runs before the first call frame and applies each valid
    /// authorization in order, potentially modifying the same authority account multiple
    /// times (e.g. auth2 sets wallet2→setterCC, then auth4 overwrites wallet2→setterBB).
    ///
    /// A state-comparison approach (original_info vs current info) only sees the NET change
    /// and loses intermediate states. We replay the auth list locally, tracking per-authority
    /// state initialized from `original_info`, to emit one code change and one nonce change
    /// per applied auth in chronological order.
    ///
    /// After the replay, the caller advances `journal_processed_up_to` past the
    /// EIP-7702 journal entries so that `process_journal_changes` won't re-process them.
    ///
    /// Must be called at root-call entry (depth=0) AFTER `on_call_enter`.
    fn process_eip7702_auth_list<CTX>(&mut self, context: &mut CTX)
    where
        CTX: ContextTr,
        CTX::Journal: JournalExt,
    {
        use reth_revm::revm::{
            context_interface::{transaction::AuthorizationTr, Cfg, Transaction},
            Database as _,
        };

        if context.tx().authorization_list_len() == 0 {
            return;
        }

        let chain_id = context.cfg().chain_id();

        // Collect auth list to avoid simultaneous borrows on `context.tx()` and
        // `context.journal()` inside the loop.
        let auths: Vec<(Option<Address>, U256, u64, Address)> = context
            .tx()
            .authorization_list()
            .map(|a| (a.authority(), a.chain_id(), a.nonce(), a.address()))
            .collect();

        // Per-authority running state: (nonce, code_hash, code_bytes).
        // Initialized lazily from original_info on first auth for that address.
        let mut auth_tracker: HashMap<Address, (u64, B256, Vec<u8>)> = HashMap::new();

        // The sender's nonce is bumped by deduct_caller BEFORE apply_auth_list runs.
        // original_info.nonce still holds the pre-tx nonce, so for the sender we must
        // add 1 to match the nonce that apply_auth_list will see.
        let tx_sender = context.tx().caller();

        for (maybe_authority, auth_chain_id, auth_nonce, target_address) in auths {
            // 1. Chain ID must be zero (wildcard) or match the current chain.
            if !auth_chain_id.is_zero() && auth_chain_id != U256::from(chain_id) {
                continue;
            }

            // 2. Nonce must be < u64::MAX.
            if auth_nonce == u64::MAX {
                continue;
            }

            // 3. Authority must be recoverable via ecrecover.
            let authority = match maybe_authority {
                Some(a) => a,
                None => continue,
            };

            // Initialize tracker for this authority from original_info on first encounter.
            if !auth_tracker.contains_key(&authority) {
                // First, read nonce and code_hash from evm_state (immutable borrow on journal).
                let (mut nonce, code_hash, code_loaded) = if let Some(acc) =
                    context.journal().evm_state().get(&authority)
                {
                    let code = acc.original_info.code.as_ref().map(|b| b.original_bytes().to_vec());
                    (acc.original_info.nonce, acc.original_info.code_hash, code)
                } else {
                    (0, KECCAK_EMPTY, Some(Vec::new()))
                };

                // Sender's nonce was already incremented by deduct_caller.
                if authority == tx_sender {
                    nonce += 1;
                }

                // If code wasn't loaded in original_info (common when load_account skips
                // code loading), fetch it from the database via code_by_hash.
                let code_bytes = match code_loaded {
                    Some(bytes) => bytes,
                    None if code_hash == KECCAK_EMPTY => Vec::new(),
                    None => context
                        .db_mut()
                        .code_by_hash(code_hash)
                        .ok()
                        .map(|b| b.original_bytes().to_vec())
                        .unwrap_or_default(),
                };

                auth_tracker.insert(authority, (nonce, code_hash, code_bytes));
            }
            let (tracked_nonce, tracked_code_hash, tracked_code) =
                auth_tracker.get_mut(&authority).unwrap();

            // 4. Code must be empty or an EIP-7702 delegation designator.
            let code_eligible = *tracked_code_hash == KECCAK_EMPTY
                || (tracked_code.len() == 23 && tracked_code.starts_with(&[0xef, 0x01, 0x00]));
            if !code_eligible {
                continue;
            }

            // 5. Nonce in the auth must match the authority's current nonce.
            if auth_nonce != *tracked_nonce {
                continue;
            }

            // Valid auth: compute the new code that delegate() will set.
            let (new_hash, new_code) = if target_address.is_zero() {
                (KECCAK_EMPTY, Vec::new())
            } else {
                let mut raw = [0u8; 23];
                raw[0] = 0xef;
                raw[1] = 0x01;
                raw[2] = 0x00;
                raw[3..].copy_from_slice(target_address.as_slice());
                let hash = alloy_primitives::keccak256(raw);
                (hash, raw.to_vec())
            };

            let old_hash = *tracked_code_hash;
            let old_code = tracked_code.clone();
            let old_nonce = *tracked_nonce;

            // Emit code change only when the hash actually differs.
            if old_hash != new_hash {
                self.tracer.on_code_change(authority, old_hash, new_hash, &old_code, &new_code);
            }
            // Always emit nonce change for each applied auth.
            self.tracer.on_nonce_change(authority, old_nonce, old_nonce + 1);

            // Advance per-authority tracker.
            *tracked_nonce = old_nonce + 1;
            *tracked_code_hash = new_hash;
            *tracked_code = new_code;

            // Note: journal_processed_up_to is advanced after this function returns,
            // so process_journal_changes will skip the CodeChange/NonceBump entries
            // that revm emitted for this auth.
        }
    }

    /// Capture nonce/code state for self-destructed accounts at root call exit.
    ///
    /// In Geth 1.17.x, `statedb.Finalise()` emits `OnNonceChange` (nonce→0) and
    /// `OnCodeChange` (code→empty) for each destroyed account AFTER post-tx balance
    /// changes (gas refund, coinbase reward). In revm, no journal entries are pushed
    /// for these cleanup operations. We capture the pre-destruction state here (at root
    /// call exit, while EVM context is still available) and emit later in
    /// `process_post_tx_balance_changes` to match Geth's ordinal ordering.
    fn capture_selfdestruct_cleanup<CTX>(&mut self, context: &mut CTX)
    where
        CTX: ContextTr,
        CTX::Journal: JournalExt,
    {
        for &address in self.selfdestruct_addresses.iter() {
            if let Some(account) = context.journal().evm_state().get(&address) {
                self.pending_selfdestruct_cleanups.push(SelfdestructCleanupEntry {
                    address,
                    nonce: account.info.nonce,
                    code_hash: account.info.code_hash,
                    code: account
                        .info
                        .code
                        .as_ref()
                        .map(|b| b.original_bytes())
                        .unwrap_or_default(),
                });
            }
        }
    }

    /// Emit post-execution balance changes: gas refund to sender and miner fee to coinbase.
    ///
    /// In Geth, these are emitted by `OnBalanceChange` hooks inside `reimburse_caller` and
    /// `reward_beneficiary`. In revm, no inspector hooks fire during post_execution, so we
    /// explicitly compute and emit them using the Ethereum gas accounting rules.
    ///
    /// Must be called after `execute_transaction_without_commit` returns. Uses `balance_tracker`
    /// for the sender's last-known balance and `get_pre_tx_balance` for the coinbase's pre-tx
    /// balance (State<DB> still holds pre-tx balances at this point).
    ///
    /// Resets `balance_tracker`, `code_change_tracker`, and `journal_processed_up_to`
    /// so the inspector is ready for the next transaction.
    pub fn process_post_tx_balance_changes<F>(
        &mut self,
        sender: Address,
        coinbase: Address,
        gas_limit: u64,
        gas_used: u64,
        effective_gas_price: u128,
        base_fee: u64,
        mut get_pre_tx_balance: F,
    ) where
        F: FnMut(Address) -> U256,
    {
        use pb::sf::ethereum::r#type::v2::balance_change::Reason;

        // Gas refund to sender: reimburse unused gas at effective_gas_price.
        // gas_used from ExecutionResult already accounts for the capped refund counter,
        // so remaining_gas = gas_limit - gas_used includes both unspent gas and EVM refunds.
        let remaining_gas = gas_limit.saturating_sub(gas_used);
        if remaining_gas > 0 {
            let refund_amount = U256::from(remaining_gas) * U256::from(effective_gas_price);
            let old_balance = self
                .balance_tracker
                .get(&sender)
                .copied()
                .unwrap_or_else(|| get_pre_tx_balance(sender));
            let new_balance = old_balance + refund_amount;
            self.tracer.on_balance_change(sender, old_balance, new_balance, Reason::GasRefund);
        }

        // Coinbase reward: the priority fee portion of consumed gas.
        // Post-EIP-1559 the base fee is burned, only the tip goes to the coinbase.
        // Pre-EIP-1559 (base_fee == 0) the entire gas price goes to coinbase.
        let priority_fee_per_gas = effective_gas_price.saturating_sub(base_fee as u128);
        if gas_used > 0 && priority_fee_per_gas > 0 {
            let reward_amount = U256::from(gas_used) * U256::from(priority_fee_per_gas);
            let old_balance = self
                .balance_tracker
                .get(&coinbase)
                .copied()
                .unwrap_or_else(|| get_pre_tx_balance(coinbase));
            let new_balance = old_balance + reward_amount;
            self.tracer.on_balance_change(
                coinbase,
                old_balance,
                new_balance,
                Reason::RewardTransactionFee,
            );
        }

        // Emit nonce reset and code clearing for self-destructed accounts.
        // This matches Geth 1.17.x's Finalise() ordering: nonce/code cleanup happens
        // AFTER gas refund and coinbase reward, so ordinals are sequenced correctly.
        for entry in self.pending_selfdestruct_cleanups.drain(..) {
            if entry.nonce > 0 {
                self.tracer.on_nonce_change(entry.address, entry.nonce, 0);
            }
            if entry.code_hash != KECCAK_EMPTY {
                self.tracer.on_code_change(
                    entry.address,
                    entry.code_hash,
                    KECCAK_EMPTY,
                    entry.code.as_ref(),
                    &[],
                );
            }
        }

        self.balance_tracker.clear();
        self.selfdestruct_addresses.clear();
        self.journal_processed_up_to = 0;

        self.log_block_index += self.trx_logs_count;
        self.trx_logs_count = 0;
    }

    /// Map EVM call scheme to Firehose call type opcode
    fn map_call_type_opcode(scheme: &reth_revm::revm::interpreter::CallScheme) -> u8 {
        use reth_revm::revm::interpreter::CallScheme;
        match scheme {
            CallScheme::Call => Opcode::Call as u8,
            CallScheme::CallCode => Opcode::CallCode as u8,
            CallScheme::DelegateCall => Opcode::DelegateCall as u8,
            CallScheme::StaticCall => Opcode::StaticCall as u8,
        }
    }

    /// Format EVM execution failure reason to match Geth's error strings.
    ///
    /// `is_create` distinguishes CREATE context (where OOG produces
    /// "contract creation code storage out of gas") from CALL context ("out of gas").
    ///
    /// Geth reference: go-ethereum/core/vm/errors.go
    fn failure_reason(
        result: reth_revm::revm::interpreter::InstructionResult,
        is_create: bool,
    ) -> StringError {
        use reth_revm::revm::interpreter::InstructionResult;
        StringError(match result {
            // Revert variants
            InstructionResult::Revert => "execution reverted".to_string(),
            InstructionResult::CallTooDeep => "max call depth exceeded".to_string(),
            InstructionResult::OutOfFunds => "insufficient balance for transfer".to_string(),
            InstructionResult::CreateInitCodeStartingEF00
            | InstructionResult::InvalidEOFInitCode
            | InstructionResult::InvalidExtDelegateCallTarget => "execution reverted".to_string(),

            // Out-of-gas variants — Geth distinguishes CREATE vs CALL context
            InstructionResult::OutOfGas
            | InstructionResult::MemoryOOG
            | InstructionResult::MemoryLimitOOG
            | InstructionResult::PrecompileOOG
            | InstructionResult::InvalidOperandOOG
            | InstructionResult::ReentrancySentryOOG => {
                if is_create {
                    "contract creation code storage out of gas".to_string()
                } else {
                    "out of gas".to_string()
                }
            }

            // Specific error variants with known Geth equivalents
            InstructionResult::InvalidFEOpcode => "invalid opcode: INVALID".to_string(),
            InstructionResult::InvalidJump => "invalid jump destination".to_string(),
            InstructionResult::StackOverflow => "stack limit reached 1024 (1023)".to_string(),
            InstructionResult::StackUnderflow => "stack underflow".to_string(),
            InstructionResult::CallNotAllowedInsideStatic
            | InstructionResult::StateChangeDuringStaticCall => "write protection".to_string(),
            InstructionResult::CreateCollision => "contract address collision".to_string(),
            InstructionResult::CreateContractSizeLimit => "max code size exceeded".to_string(),
            InstructionResult::CreateContractStartingWithEF => {
                "invalid code: must not begin with 0xef".to_string()
            }
            InstructionResult::CreateInitCodeSizeLimit => "max initcode size exceeded".to_string(),
            InstructionResult::NonceOverflow => "nonce uint64 overflow".to_string(),

            // Precompile errors — best effort, the specific error message (e.g. "point is
            // not on curve") is lost by the time we reach call_end since revm collapses it
            // into a single PrecompileError variant. We use the humanized form as fallback.
            InstructionResult::PrecompileError => "precompile error".to_string(),

            // Fallback: humanize CamelCase enum variant (e.g. "OutOfOffset" → "out of offset")
            other => humanize_instruction_result(other),
        })
    }
}

/// Converts a CamelCase enum Debug representation into lowercase words.
/// e.g. `NotActivated` → `"not activated"`, `FatalExternalError` → `"fatal external error"`
fn humanize_instruction_result(result: reth_revm::revm::interpreter::InstructionResult) -> String {
    let name = format!("{:?}", result);
    let mut words = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            words.push(' ');
        }
        words.push(ch.to_ascii_lowercase());
    }
    words
}

impl<'a, CTX> Inspector<CTX, EthInterpreter> for FirehoseInspector<'a>
where
    CTX: ContextTr,
    CTX::Journal: JournalExt,
{
    /// Called before each opcode executes (equivalent to Geth's OnOpcode hook)
    fn step(&mut self, interp: &mut Interpreter<EthInterpreter>, context: &mut CTX) {
        // On the first step of a new call frame, process journal changes to capture
        // the value transfer BalanceTransfer pushed by frame_init after call/create returned.
        // This must happen before any revert could remove the entry.
        if self.pending_value_transfer_check {
            self.pending_value_transfer_check = false;
            self.process_journal_changes(context);
        }

        let journal = context.journal();

        let pc = interp.bytecode.pc() as u64;
        let op = interp.bytecode.opcode();
        let gas = interp.gas.remaining();
        let depth = journal.depth() as i32;

        self.last_step =
            Some(StepContext { start_journal_idx: journal.journal().len(), opcode: op });

        self.tracer.on_opcode(pc, op, gas, 0, &[], depth, None);

        if op == Opcode::Keccak256 as u8 {
            Self::step_keccak256(&mut self.tracer, interp);
        }
    }

    /// Called after each opcode executes; used to detect SSTORE and SELFDESTRUCT state changes.
    fn step_end(&mut self, _interp: &mut Interpreter<EthInterpreter>, context: &mut CTX) {
        let step_ctx = match self.last_step.take() {
            Some(ctx) => ctx,
            None => return,
        };

        use reth_revm::revm::context::JournalEntry;

        if step_ctx.opcode == Opcode::Sstore as u8 {
            let journal = context.journal();
            let new_entries = &journal.journal()[step_ctx.start_journal_idx..];
            for entry in new_entries {
                if let JournalEntry::StorageChanged { address, key, had_value } = entry {
                    let new_value =
                        context.journal().evm_state()[address].storage[key].present_value();
                    self.tracer.on_storage_change(
                        *address,
                        B256::from(key.to_be_bytes::<32>()),
                        B256::from(had_value.to_be_bytes::<32>()),
                        B256::from(new_value.to_be_bytes::<32>()),
                    );
                }
            }
        } else if step_ctx.opcode == Opcode::SelfDestruct as u8 {
            self.process_selfdestruct_balance_changes(context, step_ctx.start_journal_idx);
        }
    }

    /// CALL, CALLCODE, DELEGATECALL, or STATICCALL is made
    fn call(&mut self, context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        use reth_revm::revm::interpreter::CallScheme;

        let depth = context.journal().depth() as i32;
        let call_type = Self::map_call_type_opcode(&inputs.scheme);

        // revm's CallInputs field semantics differ from Geth's for delegate-style calls:
        //   - caller:           preserved msg.sender from parent context
        //   - target_address:   the contract whose storage is used (the delegating contract)
        //   - bytecode_address: the contract whose code actually executes
        //
        // Geth/Firehose expects:
        //   - caller: the contract that issued the DELEGATECALL (= target_address)
        //   - address: the contract whose code runs (= bytecode_address)
        //
        // CALLCODE is similar but only the address differs (caller is already correct).
        let (from, to) = match inputs.scheme {
            CallScheme::DelegateCall => (inputs.target_address, inputs.bytecode_address),
            CallScheme::CallCode => (inputs.caller, inputs.bytecode_address),
            _ => (inputs.caller, inputs.target_address),
        };

        log_journal("call_enter", context);

        if depth == 0 {
            // At depth 0 (root call entry), the journal contains entries from deduct_caller
            // (BalanceChange for gas cost, NonceBump for CALL transactions) and load_accounts
            // (AccountWarmed for coinbase/access-list). We skip process_journal_changes here
            // because deduct_caller's BalanceChange would be emitted with the wrong reason
            // (Transfer instead of GasBuy). Instead, advance past all pre-execution journal
            // entries and emit gas buy + nonce explicitly with the correct reasons.
            self.journal_processed_up_to = context.journal().journal().len();

            if let Some(account) = context.journal().evm_state().get(&inputs.caller) {
                // Gas buy: sender's balance decreased by gas_limit * effective_gas_price
                let old_balance = account.original_info.balance;
                let new_balance = account.info.balance;
                if old_balance != new_balance {
                    self.tracer.on_balance_change(
                        inputs.caller,
                        old_balance,
                        new_balance,
                        pb::sf::ethereum::r#type::v2::balance_change::Reason::GasBuy,
                    );
                    self.balance_tracker.insert(inputs.caller, new_balance);
                }

                // Nonce bump from deduct_caller (CALL transactions) or
                // from original state for CREATE (nonce bump happens later
                // in create_account_checkpoint).
                let old_nonce = account.original_info.nonce;
                let new_nonce = account.info.nonce;
                if old_nonce != new_nonce {
                    self.tracer.on_nonce_change(inputs.caller, old_nonce, new_nonce);
                }
            }
        } else {
            // Process journal entries BEFORE pushing the child call. This ensures that
            // entries from the parent's execution (including the parent call's own value
            // transfer BalanceTransfer) are attributed to the parent call, not the child.
            //
            // In Geth, OnEnter fires first (pushing the call), then Transfer runs and
            // OnBalanceChange fires (on the newly-pushed call). revm's journal captures
            // the same entries but they're only visible at the NEXT inspector hook. By
            // processing here (before pushing), the parent's BalanceTransfer from a
            // previous call setup lands on the parent. The current call's own value
            // transfer will be created by revm AFTER this hook returns and processed
            // at the next call/call_end, correctly landing on THIS call.
            self.process_journal_changes(context);
        }

        self.tracer.on_call_enter(
            depth,
            call_type,
            from,
            to,
            inputs.input.bytes(context).as_ref(),
            inputs.gas_limit,
            inputs.value.get(),
        );

        // EIP-7702: override address_delegates_to using the live EVM state.
        // on_call_enter uses the pre-block state reader which misses delegations committed
        // by earlier transactions in the same block. At call-hook time, first_frame_input
        // has already loaded the 'to' account with code, so evm_state() reflects the
        // in-block delegation set by any prior transaction.
        {
            if let Some(account) = context.journal().evm_state().get(&to) {
                if let Some(eip7702) = account.info.code.as_ref().and_then(|code| match code {
                    reth_revm::bytecode::Bytecode::Eip7702(eip) => Some(eip.address()),
                    _ => None,
                }) {
                    self.tracer.set_current_call_address_delegates_to(eip7702);
                }
            }
        }

        // At root call entry, replay the EIP-7702 auth list to emit one code change and
        // one nonce change per applied auth in chronological order. This correctly captures
        // intermediate states when the same authority appears multiple times in the list.
        if depth == 0 {
            self.process_eip7702_auth_list(context);
            // Advance past EIP-7702 journal entries so process_journal_changes won't
            // re-process the CodeChange/NonceBump entries we just handled.
            self.journal_processed_up_to = context.journal().journal().len();
        }

        // After this hook returns, revm's frame_init will push a BalanceTransfer for the
        // value transfer (if any). Set flag so the first `step` picks it up.
        self.pending_value_transfer_check = true;

        None
    }

    /// CALL* operation completes
    fn call_end(&mut self, context: &mut CTX, _inputs: &CallInputs, outcome: &mut CallOutcome) {
        log_journal("call_exit", context);

        // Scan journal entries accumulated during this call's execution BEFORE popping it,
        // so changes are attributed to the call that caused them.
        self.process_journal_changes(context);

        // Clear pending flag: if the call failed before executing any opcode (e.g.
        // OutOfFunds, CallTooDeep), step never ran to clear it.
        self.pending_value_transfer_check = false;

        let depth = context.journal().depth() as i32;
        let failed = !outcome.result.is_ok();
        let is_revert = outcome.result.result.is_revert();
        let err: Option<StringError> =
            if failed { Some(Self::failure_reason(outcome.result.result, false)) } else { None };

        // EVM semantics: a halting error (not a revert) consumes all gas
        // allocated to the call. revm's gas.spent() only tracks opcodes that
        // actually executed, so we use gas.limit for non-revert failures.
        // Reverts only consume gas actually spent (remaining gas is returned).
        let gas_used = if failed && !is_revert {
            outcome.result.gas.limit()
        } else {
            outcome.result.gas.spent()
        };

        // At root call exit, capture nonce/code state for self-destructed contracts.
        // The actual emission happens later in process_post_tx_balance_changes (after
        // gas refund and coinbase reward) to match Geth 1.17.x Finalise ordinal timing.
        if depth == 0 && !self.selfdestruct_addresses.is_empty() {
            self.capture_selfdestruct_cleanup(context);
        }

        // The `reverted` parameter in on_call_exit means "did the call fail"
        // (any failure), not specifically "was it a REVERT opcode". The tracer
        // internally distinguishes reverts from other failures via the error string.
        self.tracer.on_call_exit(
            depth,
            outcome.result.output.as_ref(),
            gas_used,
            err.as_ref().map(|e| e as &dyn std::error::Error),
            failed,
        );
    }

    /// CREATE or CREATE2 is made
    fn create(&mut self, context: &mut CTX, inputs: &mut CreateInputs) -> Option<CreateOutcome> {
        use reth_revm::revm::context_interface::CreateScheme;

        let depth = context.journal().depth() as i32;
        let (call_type, created_address) = match inputs.scheme() {
            CreateScheme::Create2 { .. } => {
                // CREATE2 address is deterministic, no nonce needed
                (Opcode::Create2 as u8, inputs.created_address(0))
            }
            _ => {
                // CREATE address requires caller nonce
                let nonce = context
                    .journal_mut()
                    .load_account(inputs.caller())
                    .map(|acc| acc.info.nonce)
                    .unwrap_or(0);
                (Opcode::Create as u8, inputs.created_address(nonce))
            }
        };

        log_journal("create_enter", context);

        // For root-level CREATE/CREATE2 (depth 0), the TxEvent.to was None (contract creation),
        // leaving the transaction trace's `to` field empty. Patch it now that we know the address.
        if depth == 0 {
            self.tracer.set_transaction_to(created_address);
        }

        if depth == 0 {
            // Same rationale as in `call` hook: skip process_journal_changes at depth 0
            // to avoid double-emitting deduct_caller's BalanceChange/NonceBump with wrong
            // reasons. Emit gas buy + nonce explicitly instead.
            //
            // For CREATE, deduct_caller does NOT bump the nonce (only CALL does).
            // create_account_checkpoint will bump the nonce later and DOES push a
            // NonceChange journal entry that process_journal_changes will pick up.
            self.journal_processed_up_to = context.journal().journal().len();

            if let Some(account) = context.journal().evm_state().get(&inputs.caller()) {
                // Gas buy balance change
                let old_balance = account.original_info.balance;
                let new_balance = account.info.balance;
                if old_balance != new_balance {
                    self.tracer.on_balance_change(
                        inputs.caller(),
                        old_balance,
                        new_balance,
                        pb::sf::ethereum::r#type::v2::balance_change::Reason::GasBuy,
                    );
                    self.balance_tracker.insert(inputs.caller(), new_balance);
                }

                // Nonce bump from deduct_caller (only for CALL txs; for CREATE txs
                // the nonce hasn't been bumped yet by deduct_caller, so old == new).
                let old_nonce = account.original_info.nonce;
                let new_nonce = account.info.nonce;
                if old_nonce != new_nonce {
                    self.tracer.on_nonce_change(inputs.caller(), old_nonce, new_nonce);
                }
            }
        } else {
            // Process journal entries BEFORE pushing child (same rationale as in `call` hook).
            self.process_journal_changes(context);
        }

        self.tracer.on_call_enter(
            depth,
            call_type,
            inputs.caller(),
            created_address,
            inputs.init_code(),
            inputs.gas_limit(),
            inputs.value(),
        );

        // After this hook returns, revm's frame_init will push a BalanceTransfer for the
        // value transfer (if any). Set flag so the first `step` picks it up.
        self.pending_value_transfer_check = true;

        None
    }

    /// CREATE* operation completes
    fn create_end(
        &mut self,
        context: &mut CTX,
        _inputs: &CreateInputs,
        outcome: &mut CreateOutcome,
    ) {
        use reth_revm::revm::interpreter::InstructionResult;

        log_journal("create_exit", context);

        // Scan journal entries accumulated during this create's execution (including code
        // deployment) BEFORE popping the call, so changes are attributed to the CREATE
        // call.
        self.process_journal_changes(context);

        // Clear pending flag: if the CREATE failed before executing any opcode (e.g.
        // OutOfFunds, CallTooDeep, CreateCollision), step never ran to clear it.
        self.pending_value_transfer_check = false;

        // Emit the created contract's nonce change (0→1, EIP-161). This is set directly
        // by create_account_checkpoint (target_acc.info.nonce = 1) WITHOUT pushing a
        // NonceBump journal entry, so process_journal_changes will never pick it up.
        // If the CREATE failed, the journal checkpoint was reverted but the nonce change
        // still needs to be recorded in the Firehose trace (stateReverted captures it).
        //
        // Skip for failures that occur BEFORE create_account_checkpoint runs:
        // - CallTooDeep / OutOfFunds: frame setup aborted before checkpoint
        // - CreateCollision / OverflowPayment: create_account_checkpoint itself failed
        let skip_created_nonce = matches!(
            outcome.result.result,
            InstructionResult::CallTooDeep
                | InstructionResult::OutOfFunds
                | InstructionResult::CreateCollision
                | InstructionResult::OverflowPayment
        );
        if !skip_created_nonce {
            if let Some(address) = outcome.address {
                self.tracer.on_nonce_change(address, 0, 1);
            }
        }

        let depth = context.journal().depth() as i32;
        let failed = !outcome.result.is_ok();
        let is_revert = outcome.result.result.is_revert();
        let err: Option<StringError> =
            if failed { Some(Self::failure_reason(outcome.result.result, true)) } else { None };

        let gas_used = if failed && !is_revert {
            outcome.result.gas.limit()
        } else {
            outcome.result.gas.spent()
        };

        // At root create exit, capture nonce/code state for self-destructed contracts
        // (same rationale as in call_end — emission deferred to process_post_tx_balance_changes).
        if depth == 0 && !self.selfdestruct_addresses.is_empty() {
            self.capture_selfdestruct_cleanup(context);
        }

        self.tracer.on_call_exit(
            depth,
            outcome.result.output.as_ref(),
            gas_used,
            err.as_ref().map(|e| e as &dyn std::error::Error),
            failed,
        );
    }

    /// LOG operation is executed
    fn log_full(
        &mut self,
        _interp: &mut Interpreter<EthInterpreter>,
        context: &mut CTX,
        log: AlloyLog,
    ) {
        // The journal tracks all non-reverted logs. log_full fires after the
        // log is appended, so logs().len() - 1 is this log's index in the transaction.
        // On revert, the journal truncates logs back, so subsequent logs after
        // a revert get correct indices automatically.
        //
        self.trx_logs_count = (context.journal().logs().len() as u32);
        let block_index = self.trx_logs_count.saturating_sub(1) + self.log_block_index;
        self.tracer.on_log(log.address, log.topics(), &log.data.data, block_index);
    }

    /// SELFDESTRUCT is executed
    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        // Note: selfdestruct_addresses is populated in process_selfdestruct_balance_changes
        // (only for AccountDestroyed entries, not BalanceTransfer), because post-Cancun
        // non-locally-created contracts are NOT destroyed and don't need cleanup.

        // In Geth's tracer, SELFDESTRUCT is modelled as a nested call at depth+1.
        // on_call_enter with OP_SELFDESTRUCT sets the `latest_call_enter_suicided` flag
        // and on_call_exit immediately clears it (no-op on call stack).
        // Depth doesn't affect SELFDESTRUCT handling so we use 1 (any non-zero value).
        self.tracer.on_call_enter(1, Opcode::SelfDestruct as u8, contract, target, &[], 0, value);
        self.tracer.on_call_exit(1, &[], 0, None, false);
    }
}

/// Logs the current journal entries (since the last checkpoint) using firehose trace-level logging.
///
/// The journal records state mutations made by the EVM: balance transfers, nonce bumps, storage
/// writes, account creation/warming, etc. This function is meant to be called at interesting
/// points during execution (e.g. before/after call/create) to aid debugging.
pub fn log_journal<CTX>(label: &str, context: &CTX)
where
    CTX: ContextTr,
    CTX::Journal: JournalExt,
{
    use reth_revm::revm::context::JournalEntry;

    if !firehose_tracer::logging::is_firehose_debug_enabled() {
        return;
    }

    let journal = context.journal().journal();
    if journal.is_empty() {
        firehose_tracer::firehose_debug!("{}: journal empty", label);
        return;
    }

    firehose_tracer::firehose_debug!("{}: journal ({} entries)", label, journal.len());
    for (i, entry) in journal.iter().enumerate() {
        match entry {
            JournalEntry::AccountTouched { address } => {
                firehose_tracer::firehose_debug!("  [{i}] AccountTouched addr={address}");
            }
            JournalEntry::AccountDestroyed { address, target, had_balance, .. } => {
                firehose_tracer::firehose_debug!(
                    "  [{i}] AccountDestroyed addr={address} target={target} balance={had_balance}"
                );
            }
            JournalEntry::BalanceChange { address, old_balance } => {
                firehose_tracer::firehose_debug!(
                    "  [{i}] BalanceChange addr={address} old={old_balance}"
                );
            }
            JournalEntry::BalanceTransfer { from, to, balance } => {
                firehose_tracer::firehose_debug!(
                    "  [{i}] BalanceTransfer from={from} to={to} amount={balance}"
                );
            }
            JournalEntry::NonceChange { address, previous_nonce } => {
                firehose_tracer::firehose_debug!(
                    "  [{i}] NonceChange addr={address} prev_nonce={previous_nonce}"
                );
            }
            JournalEntry::NonceBump { address } => {
                firehose_tracer::firehose_debug!("  [{i}] NonceBump addr={address}");
            }
            JournalEntry::AccountCreated { address, is_created_globally } => {
                firehose_tracer::firehose_debug!(
                    "  [{i}] AccountCreated addr={address} global={is_created_globally}"
                );
            }
            JournalEntry::StorageChanged { address, key, had_value } => {
                firehose_tracer::firehose_debug!(
                    "  [{i}] StorageChanged addr={address} key={key} had={had_value}"
                );
            }
            JournalEntry::CodeChange { address } => {
                firehose_tracer::firehose_debug!("  [{i}] CodeChange addr={address}");
            }
            // Skip warm/cold tracking and transient storage — not relevant for Firehose
            _ => {}
        }
    }
}

/// Logs the EvmState (accounts and their info) using firehose trace-level logging.
///
/// This logs all accounts that have been touched/modified in the state, along with their
/// balance, nonce, code hash, and status flags. Useful for inspecting the full state picture
/// at a given point (e.g. via the OnStateHook after each transaction/system call).
pub fn log_evm_state(label: &str, state: &reth_revm::revm::state::EvmState) {
    if !firehose_tracer::logging::is_firehose_debug_enabled() {
        return;
    }

    if state.is_empty() {
        firehose_tracer::firehose_debug!("{}: evm_state empty", label);
        return;
    }

    firehose_tracer::firehose_debug!("{}: evm_state ({} accounts)", label, state.len());
    for (addr, account) in state {
        let info = &account.info;
        let storage_count = account.storage.len();
        firehose_tracer::firehose_debug!(
            "  {addr} balance={} nonce={} code_hash={} status={:?} storage_slots={storage_count}",
            info.balance,
            info.nonce,
            info.code_hash,
            account.status,
        );
    }
}
