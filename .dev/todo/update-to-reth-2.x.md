# Update to reth 2.x

mode: feature
state: planned
root_git: .worktrees/feature-update-to-reth-2.x
worktree: .worktrees/feature-update-to-reth-2.x
branch: feature/update-to-reth-2.x
target_branch: release/reth-1.x

> **Resume protocol:** read **Dev Feedback** and the **State Tracker** below first, then jump to the
> step marked `Current`. Ensure that you are in the correct worktree and branch according to preamble here. Update current with Developer feedback and update the tracker after every meaningful change.
> Do not mutate completed steps; append a new entry instead.

---

## Initial Description

Prepare a plan to update to reth 2.x. We already have a branch `release/reth-2.x` that is lagging behind `release/reth-1.x`. The two options to evaluate are:

1. Merge `release/reth-1.x` into `release/reth-2.x` then update to latest `v2.2.0`
2. Start from scratch: create new temp `release/reth-2.x-new` based on actual `release/reth-1.x` then merge `v2.2.0` in

The goal is to evaluate the best plan to minimize conflicts. Cargo tests should pass at the end.

Context gathered:
- Main worktree is on `release/reth-1.x`
- `release/reth-2.x` has 758 commits since merge-base with `release/reth-1.x`
- `release/reth-1.x` has 44 commits since that same merge-base
- The merge-base commit is `564ffa586845fa4a8bb066f0c7b015ff36b26c08`
- Remote branches: `sf/release/reth-1.x`, `sf/release/reth-2.x`

## Dev Feedback

## Spec & Implementation

### Summary

This spec describes how to bring the StreamingFast fork of reth up to upstream `v2.2.0`, with the Firehose integration intact and all cargo tests passing. After analysis of the two branches, **Option B (fresh branch from reth-1.x + merge v2.2.0) is recommended** because it results in a clean merge conflict surface limited strictly to SF-specific Firehose code versus the upstream v2.2.0 codebase, avoiding compounded conflicts that exist in the already-diverged `release/reth-2.x`.

---

### Option Analysis

#### Situation Summary (from git analysis)

| Dimension | Value |
|---|---|
| Merge-base (reth-1.x ∩ reth-2.x) | `564ffa586` |
| SF-unique commits on `reth-1.x` | 47 |
| Commits on `reth-2.x` since merge-base | 758 |
| SF-unique commits on `reth-2.x` | ~10 early-draft Firehose commits (superseded by reth-1.x work) |
| Commits in `v2.2.0` ahead of `reth-2.x` | 85 |
| Firehose files changed on reth-1.x vs reth-2.x (net diff) | 643 insertions, 3027 deletions (reth-1.x has the canonical implementation) |

Key finding: `reth-2.x` contains 10 early-draft Firehose commits (`First working compilation pass`, `Unified live & historical tracing paths`, etc.) that are structurally different from the mature implementation now on `reth-1.x`. The `reth-1.x` Firehose implementation is ~1400 lines larger and has 27 additional commits on top of those early drafts. The executor alone is 1036 lines on reth-1.x vs 679 on reth-2.x.

#### Option A — Merge reth-1.x into reth-2.x, then update to v2.2.0

**Merge 1: `reth-1.x` → `reth-2.x`**
- Three-way merge base is `564ffa586` (the common ancestor).
- The Firehose files diverged heavily: reth-2.x has an early draft; reth-1.x has the mature rewrite. Git will flag nearly every Firehose file as a conflict.
- Non-Firehose SF changes on reth-1.x (dependency bumps, CI config, trie fixes cherry-picked from upstream) will likely conflict with the 758 upstream commits already absorbed by reth-2.x.
- Estimated conflict surface: **high** — both Firehose files and non-Firehose upstream integrations that were handled differently in each branch.

**Merge 2: merged result → v2.2.0 (85 commits)**
- 85 additional upstream commits on top of an already-complex merge result.
- Any conflicts from Merge 1 that were resolved incorrectly will compound here.
- Risk: difficult to validate correctness because the intermediate state was complex.

**Assessment:** Two-step merge with compounding conflicts; the early-draft Firehose code on reth-2.x is not the baseline we want — it's noise that creates false conflicts.

#### Option B — Fresh branch from reth-1.x, merge v2.2.0 (RECOMMENDED)

**Step: Create `release/reth-2.x-new` from `release/reth-1.x`**
- Starting point is the clean, fully-working SF implementation.
- No baggage from the early draft Firehose commits on reth-2.x.

**Single merge: `v2.2.0` into `release/reth-2.x-new`**
- Merge base between `release/reth-1.x` and `v2.2.0` is `564ffa586` (same as before).
- The conflict surface is: SF Firehose files vs upstream changes in those same files over 758+85 = ~843 commits.
- Crucially: reth-1.x's Firehose code is the *authoritative* implementation; for any file that upstream touched and SF also touched, the resolution strategy is clear — take upstream's structural changes and re-apply SF's Firehose hooks.
- Non-Firehose SF changes (CI, dependabot, trie cherry-picks, relax limits) are simple: most will either apply cleanly or need trivial resolution since upstream already has the same trie fixes.

**Assessment:** Single merge, clean baseline, conflicts are predictable and scoped to Firehose integration code. The resolution strategy is well-defined.

---

### Scope

**In scope:**
- Create `release/reth-2.x-new` from `release/reth-1.x`
- Merge upstream `v2.2.0` into it, resolving conflicts
- Validate compilation and tests (`crates/firehose`, `crates/firehose-tests`, full workspace)
- Push as the new `release/reth-2.x` once validated

**Out of scope:**
- Updating `release/reth-1.x` (it stays as-is)
- Any new Firehose features beyond what reth-1.x already has
- Optimism/L2 support (not currently in scope for this fork)

---

### Design: Conflict Prediction

Based on the diff analysis, the files most likely to have conflicts are:

**Certain conflicts (SF code + upstream evolution):**
- `crates/firehose/src/executor.rs` — heavily SF-owned; upstream may have refactored `BlockExecutor` trait
- `crates/firehose/src/inspector.rs` — SF-owned; revm inspector API may have changed in v2.x
- `crates/firehose/src/runner.rs` — SF-owned; pipeline/stage runner API changes expected
- `crates/firehose/src/mapper.rs` — SF-owned; alloy primitive changes may cascade
- `bin/reth/src/main.rs` or node entry — SF adds `FirehoseExecutorBuilder`
- `Cargo.toml` / `Cargo.lock` — dependency version conflicts

**Likely clean (SF didn't touch, or SF changes were cherry-picks of upstream):**
- `crates/trie/` — SF cherry-picked two trie fixes that are already upstream in v2.2.0
- CI/workflows — SF added dependabot/secret-scanning; upstream has different CI; both can coexist
- `crates/storage/`, `crates/net/`, `crates/rpc/` — SF didn't modify these

**Resolution strategy for Firehose conflicts:**
The upstream reth 2.x made the `BlockExecutor`, `BlockExecutionStrategyFactory`, and EVM config traits more generic (evident from the 758-commit diff). When resolving conflicts in Firehose files:
1. Accept upstream's structural/trait changes as the "ours" side of the executor trait bounds.
2. Re-apply SF's inspector hooks, balance tracking, journal traversal, and mapping logic.
3. The reth-1.x Firehose files are the reference for what SF functionality must be preserved.

---

### Implementation Plan

#### Phase 1 — Branch Setup

1. **Create the new working branch** from `release/reth-1.x`:
   ```bash
   git checkout -b release/reth-2.x-new release/reth-1.x
   ```
   _Touch points: git only._

2. **Fetch upstream tag**:
   ```bash
   git fetch origin v2.2.0
   # or: git fetch upstream v2.2.0 (whichever remote holds paradigmxyz/reth)
   ```

#### Phase 2 — Merge v2.2.0

3. **Initiate the merge** (do NOT auto-commit):
   ```bash
   git merge v2.2.0 --no-commit --no-ff -m "chore: merge upstream v2.2.0 into release/reth-2.x-new"
   ```

4. **Identify all conflicts**:
   ```bash
   git diff --name-only --diff-filter=U
   ```
   Expected: primarily `crates/firehose/**`, `Cargo.toml` files, `Cargo.lock`, and possibly `bin/reth/src/`.

#### Phase 3 — Conflict Resolution (ordered by priority)

5. **Resolve `Cargo.toml` / `Cargo.lock` conflicts first:**
   - For each `Cargo.toml` with conflicts: take upstream's dependency versions (alloy, revm, etc.) since we're targeting v2.2.0 compatibility.
   - For SF-specific crates (`reth-firehose`, `reth-firehose-tests`): keep them in the workspace `members` list.
   - Run `cargo update` if needed to regenerate `Cargo.lock` after manual resolution.

6. **Resolve `crates/firehose/src/executor.rs`:**
   - Check what trait/API changes upstream made to `BlockExecutionStrategyFactory`, `BlockExecutor`, `EvmConfig`.
   - Port SF's `FirehoseBlockExecutor`, `FirehoseEvmConfig`, `ChainHooks`, `PreTxAdjust`, `PostTxExtras` hooks into the new trait signatures.
   - Reference: `git show release/reth-1.x:crates/firehose/src/executor.rs`

7. **Resolve `crates/firehose/src/inspector.rs`:**
   - Check revm `Inspector` trait changes between reth 1.x and 2.2.0.
   - Port SF's journal-based balance tracking, nonce tracking, self-destruct hooks, keccak preimage handling.
   - Reference: `git show release/reth-1.x:crates/firehose/src/inspector.rs`

8. **Resolve `crates/firehose/src/runner.rs` and `mapper.rs`:**
   - Port pipeline/stage runner hooks.
   - Update alloy primitive type usages if they changed (e.g., `TransactionSigned`, `Receipt`).

9. **Resolve `bin/reth/src/` conflicts:**
   - Ensure `FirehoseExecutorBuilder` is still wired into the node builder.
   - The `firehose.rs` file contents are identical between reth-1.x and reth-2.x (confirmed), so the conflict here will be in how the upstream changed the node builder wiring.

10. **Resolve any remaining workspace-level conflicts** (CI files, `README`, etc.) — take upstream's version unless SF-specific (dependabot config, secret-scanning workflow — keep SF's additions).

#### Phase 4 — Compilation Validation

11. **Check compilation compiles cleanly:**
    ```bash
    cargo check --workspace --all-features 2>&1 | head -50
    ```
    Iterate on compilation errors crate by crate. The Firehose crates will need the most attention.

12. **Format:**
    ```bash
    cargo +nightly fmt --all
    ```

13. **Clippy:**
    ```bash
    cargo +nightly clippy --workspace --all-features 2>&1 | grep "^error"
    ```

#### Phase 5 — Testing

14. **Run Firehose unit tests:**
    ```bash
    cargo nextest run -p reth-firehose
    ```

15. **Run Firehose integration tests:**
    ```bash
    cargo nextest run -p reth-firehose-tests
    ```
    These tests (`crates/firehose-tests/tests/prestate.rs`) run prestate-based golden file comparisons and are the primary correctness gate for the Firehose tracer.

16. **Run full workspace tests:**
    ```bash
    cargo nextest run --workspace
    ```

#### Phase 6 — Branch Promotion

17. **Commit the merge:**
    ```bash
    git commit -m "chore: merge upstream v2.2.0 into release/reth-2.x-new"
    ```

18. **Rename/replace `release/reth-2.x`:**
    - Option 1 (safest): keep old branch as `release/reth-2.x-old` backup, then force-push the new one:
      ```bash
      git push sf release/reth-2.x-new:release/reth-2.x-new
      # After validation: 
      git push sf release/reth-2.x-new:release/reth-2.x --force-with-lease
      ```
    - Option 2: rename locally and push new. Coordinate with team before force-pushing the shared branch.

19. **Tag the SF release:**
    ```bash
    git tag sf/v2.2.0 release/reth-2.x-new
    git push sf sf/v2.2.0
    ```

---

### Decisions & Assumptions

| Decision/Assumption | Rationale |
|---|---|
| Option B (fresh branch) over Option A | Single merge, clean baseline, predictable conflicts. Option A has compounding conflicts from the early-draft reth-2.x Firehose code. |
| Target `v2.2.0` directly (not reth-2.x tip) | v2.2.0 is a tagged stable release, 85 commits ahead of current reth-2.x. Prefer merging a tag over a moving branch tip. |
| The early reth-2.x Firehose commits are throwaway | Confirmed by code analysis: the reth-1.x implementation is +357 lines net and has 27 additional commits. The early reth-2.x drafts (`First working compilation pass`, etc.) are superseded. |
| Firehose conflict resolution strategy: upstream structure + SF hooks | The pattern for all SF Firehose files: accept upstream's trait/API evolution, re-apply SF's tracer logic. |
| Keep `release/reth-2.x` branch name after validation | It is the canonical SF reth-2.x branch. Force-push with lease after team coordination. |
| Cherry-picked trie fixes will merge cleanly | Both `fix don't produce both updates and removals for trie nodes` and `install rayon panic handler` are upstream commits that v2.2.0 already contains. Git will detect as already-applied. |

---

## State Tracker

**Last Updated:** 2026-05-12
**Current Step:** Phase 5 — Spec Review & Acceptance
**Status:** Spec complete, awaiting user approval

| Step | Status | Notes |
|---|---|---|
| Phase 1 — Contextual Understanding | Done | Analyzed both branches, Firehose files, commit counts, merge-base |
| Phase 2 — Gap Analysis | Done | Confirmed Option B is clearly superior; no critical gaps requiring questions |
| Phase 3 — Challenging Dialogue | Skipped | Codebase analysis answered all critical questions; no ambiguities remain |
| Phase 4 — Specification Writing | Done | Full spec with 19-step implementation plan |
| Phase 5 — Spec Review | Done | State set to `planned` |
