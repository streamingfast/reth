# Prepare Release v2.2.0-fh

mode: feature
state: review
root_git: .
worktree: .worktrees/release-v2.2.0-fh
branch: release/v2.2.0-fh
target_branch: firehose/2.x

> **Resume protocol:** read **Dev Feedback** and the **State Tracker** below first, then jump to the
> step marked `Current`. Ensure that you are in the correct worktree and branch according to preamble here. Update current with Developer feedback and update the tracker after every meaningful change.
> Do not mutate completed steps; append a new entry instead.

---

## Initial Description

Prepare a git release tag `v2.2.0-fh` for the `firehose/2.x` branch.

This is a StreamingFast Firehose-specific release tag on top of the upstream reth `v2.2.0` tag.

### What to do

1. Create a `CHANGELOG.md` if one does not exist, documenting changes since the last firehose tag (`v1.11.4-fh-1` was the last 1.x tag; there is no prior 2.x firehose tag).

2. Key changes to document (commits since `v2.2.0` on `firehose/2.x`):
   - Updated `firehose-tracer` dependency to version 5.1.1
   - Added flashblocks support to `reth-firehose`: `start_flashblock_local`, `mark_flashblock` methods on `FirehoseBlockTracer`
   - Added `SynchronizedStdout` + stdout lock: `init_tracer` now accepts `Config` and handles stdout coordination internally
   - Exposed prestate types and helpers as `pub` in `reth-firehose-tests`
   - Various code formatting and cleanup

3. Commit the CHANGELOG.

4. Create the annotated git tag `v2.2.0-fh` pointing to the HEAD of `release/v2.2.0-fh` after the CHANGELOG commit.

### Tag naming convention

- `v2.2.0` — upstream reth release
- `v2.2.0-fh` — StreamingFast Firehose-instrumented release on top of v2.2.0

## Dev Feedback

## Spec & Implementation

Created `CHANGELOG.md` at the repo root documenting the Firehose-specific changes since the last release. The changelog follows Keep a Changelog format and covers:

- Flashblocks support (`start_flashblock_local`, `mark_flashblock` on `FirehoseBlockTracer`)
- `SynchronizedStdout` + `init_tracer` accepting `Config` directly
- Exposed prestate helpers as `pub` in `reth-firehose-tests`
- `firehose-tracer` bumped to 5.1.1

Committed `CHANGELOG.md` with message `chore: prepare v2.2.0-fh release` and created annotated tag `v2.2.0-fh`.

## State Tracker

**Last Updated:** 2026-05-20
**Current Step:** Step 4 — Tag created, ready for push
**Status:** Tag `v2.2.0-fh` created locally. Branch and tag not yet pushed (awaiting user review).
