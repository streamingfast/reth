# Update to reth 2.x

mode: feature
state: in_progress
root_git: .worktrees/feature-update-to-reth-2.x
worktree: .worktrees/feature-update-to-reth-2.x
branch: feature/update-to-reth-2.x
target_branch: release/reth-2.x (sf remote)

> **Resume protocol:** read **Dev Feedback** and the **State Tracker** below first, then jump to the
> step marked `Current`. Ensure that you are in the correct worktree and branch according to preamble here. Update current with Developer feedback and update the tracker after every meaningful change.
> Do not mutate completed steps; append a new entry instead.

---

## Initial Description

Update our SF fork of reth to upstream `v2.2.0`, preserving the Firehose integration from `release/reth-1.x`.

**Context:**
- Main worktree is on `release/reth-1.x` (the authoritative Firehose implementation)
- `origin` = `paradigmxyz/reth` (upstream)
- `sf` = `streamingfast/reth` (our fork)
- Working branch `feature/update-to-reth-2.x` is already based on `release/reth-1.x`
- Goal: merge `v2.2.0` into this branch, resolve conflicts, validate, then push as `sf/release/reth-2.x`

## Dev Feedback

Option B chosen: start from scratch (fresh branch from reth-1.x + merge v2.2.0). The working branch `feature/update-to-reth-2.x` already IS that fresh branch — it is derived from `release/reth-1.x`.

At the end, `release/reth-2.x` on the `sf` remote should be at `v2.2.0` tip with all Firehose work from `release/reth-1.x` intact.

## Spec & Implementation

### Approach (Option B — Fresh branch from reth-1.x, merge v2.2.0)

Starting point is the clean `release/reth-1.x` Firehose implementation (already checked out in this worktree). We do a single merge of the upstream `v2.2.0` tag, resolve conflicts (scope limited to Firehose integration code), validate compilation and tests, then push as `sf/release/reth-2.x`.

**Conflict resolution strategy for Firehose files:**
- Accept upstream's structural/trait changes (BlockExecutor, BlockExecutionStrategyFactory, EvmConfig, revm Inspector)
- Re-apply SF's Firehose hooks (inspector, balance tracking, journal traversal, mapping logic)
- Reference: `git show release/reth-1.x:crates/firehose/src/<file>` for authoritative SF code

### Implementation Plan

1. Fetch `v2.2.0` tag from `origin`
2. `git merge v2.2.0 --no-commit --no-ff`
3. Resolve conflicts (Cargo.toml, Cargo.lock, crates/firehose/**, bin/reth/)
4. `cargo check --workspace` — iterate until clean
5. `cargo +nightly fmt --all`
6. `cargo nextest run -p reth-firehose` and full workspace tests
7. Commit merge, push branch to `sf` as `release/reth-2.x`

---

## State Tracker

**Last Updated:** 2026-05-12
**Current Step:** Step 1 — Fetch v2.2.0 and initiate merge
**Status:** In progress

| Step | Status | Notes |
| ---- | ------ | ----- |
| Task file cleanup (Option B, reduce noise) | Done | Simplified, branch/target updated |
| Fetch v2.2.0 from origin | In Progress | Starting now |
</content>
</invoke>