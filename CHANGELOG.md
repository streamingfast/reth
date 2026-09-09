# Changelog

All notable changes to the StreamingFast Firehose fork of bnb-chain/reth are documented here.

This changelog covers Firehose-specific changes only. For upstream changes, see the
[bnb-chain/reth releases](https://github.com/bnb-chain/reth/releases).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## bnb-v0.1.1-fh3.2

### Fixed

- Stop advertising a finalized block that is not an ancestor of the block being emitted. Every
  `FIRE BLOCK` line carried the node's finalized head as of the moment the block executed, so a
  block from a side branch was published with a LIB number naming the canonical chain's block at
  that height; the consumer marked its own block at that height irreversible and then saw the
  reorg replace it. The advertised block is now the node's finalized head when that head is on
  the emitted block's own chain, the fork point where its branch left the canonical chain
  otherwise, and genesis when the branch cannot be tied to the canonical chain at all. Seen on
  BSC mainnet at block 120653740, where a four-block side branch was published with LIB
  120653741 — one of those blocks naming a height above its own number.

## Unreleased

This fork is consumed as a library by
[streamingfast/reth-bsc](https://github.com/streamingfast/reth-bsc), which builds the
Firehose-instrumented `reth-bsc` binary and Docker image; no binaries or images are
released from this repository.

### Fixed

- Include the SELFDESTRUCT refund when resolving an account's post-transaction balance. On
  the truly-destroyed path (EIP-6780: contract created in the same transaction, or
  pre-Cancun) revm credits the beneficiary in place and records the move only inside its
  `AccountDestroyed` journal entry — no `BalanceTransfer` is pushed — so the journal walk
  backing the `RewardTransactionFee` and `GasRefund` events missed it. A coinbase or sender
  that received a suicide refund then reported an `old_balance` contradicting the
  `SuicideRefund` event emitted moments earlier. Ported from streamingfast/reth
  `v2.3.0-fh-7`.

### Added

- Initial Firehose instrumentation on top of bnb-chain/reth `v0.0.10`, ported from
  streamingfast/reth `firehose/2.x` (`v2.3.0-fh-6`). Includes the `reth-firehose` crate
  (inspector, block tracer, executor wrappers, ExEx), live engine-tree tracing on both the
  standard and triedb validation paths, and pipeline (staged sync) tracing.

### Changed

- Adapted to this fork's dependency set (reth 2.2 base): revm 38 and alloy-evm 0.34, with
  `alloy-evm` patched to `streamingfast/evm` branch `sf/v0.34.0` so system calls
  (EIP-4788, EIP-2935, ...) are routed through the inspector.
- EIP-4788 / EIP-2935 system calls are reported with `gas_limit` 30000000 (the EIP-specified
  system-call gas limit, matching geth), instead of the 31566720 reported by the revm 40 based
  `firehose/2.x` fork. Golden test blocks were re-blessed accordingly.
