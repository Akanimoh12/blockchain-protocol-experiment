# EVM-NOTES — Giving Breeja an EVM

> Copy to `pow-node/EVM-NOTES.md` at M0 and fill in as you go. Target 2–4 pages.
> Delete these quote blocks before submitting. Write in your own words. Honesty about
> what doesn't work is graded *up*, not down.

**Author:** ·  **Branch:** `feat/evm` ·  **REVM:** 43.0.3 ·  **Spec:** SHANGHAI

## 1. Baseline (M0)

- Tests passing before any change: **48**
  - `node` unit tests: 13
  - `node` integration (`tests/mining.rs`): 8
  - `primitives` unit tests: 14
  - `rpc` integration (`tests/json_rpc.rs`): 13
- After M1: **61** (48 unchanged + 5 `WorldState` + 8 `BreejaDb`). Clippy clean with `-D warnings`.
- `cargo run --release -- --dev` (run after M1, which changes no behaviour; 2-second
  target, difficulty climbing from 3 bits as the miner retargets):

```text
sealed block number=1  txs=0 difficulty=3  attempts=10      took=0.00s
sealed block number=10 txs=0 difficulty=12 attempts=9317    took=0.01s
sealed block number=18 txs=0 difficulty=20 attempts=592039  took=0.41s
sealed block number=19 txs=0 difficulty=21 attempts=5690389 took=3.98s
sealed block number=20 txs=0 difficulty=21 attempts=934603  took=0.65s
```

## 2. Part 0 answers

### Q1. The three mempool rules that assumed no EVM
> Name each check (file:line), and say whether it became removed or relaxed, and why.

### Q2. Skip vs include on failure
> What the old comment argued, why it stops being true, what Ethereum does, and what you kept skipping.

### Q3. Reserving `gas_limit × price` vs charging `TRANSFER_GAS × price`
> Why the difference existed, and what happens to it with variable gas (refunds).

### Q4. Cost of per-block snapshots once storage exists
> The cost, and what a real node does instead.

## 3. What I built

> One short paragraph per milestone: what changed, which files, and anything surprising.

### State model (M1)
> Representation chosen (WorldState / code-by-hash / storage). Empty-code-hash convention. Unknown account → `None` or empty, and why.

### Execution through REVM (M2)
> How a block executes now. What REVM does that my code no longer does.

### Contracts (M3)
> Create handling, receipts, logs, reverted transactions.

### eth_call / eth_estimateGas (M4)
> Simulation design, nonce/balance policy and justification, the search, margin choice.

### New RPC methods (M5)
> getCode, getStorageAt, getLogs (topic support: yes/partial/no), gasPrice. Receipt fixes.

### Holding together (M6)
> Gas-based block filling, bloom, state root.

## 4. The learning objectives, answered from my code

1. Why the EVM needs a database interface; what `Database` and `DatabaseCommit` each do.
2. What an account is now; why `storage_root` and `code_hash` matter.
3. Where gas comes from; why estimation is a search (and the `GAS`-opcode caveat).
4. What a reverted transaction is; why receipts have `status`.
5. `eth_call` vs `eth_sendRawTransaction`: the one difference.
6. Who pays the miner; how double-payment would happen and what caught it.

## 5. What broke, and how I found it

> Table or list: symptom, which test or observation showed it, root cause, fix.

| Symptom | Caught by | Cause | Fix |
| --- | --- | --- | --- |
| | | | |

## 6. Decisions and compromises

- **Frontier consensus, Shanghai execution:** why it is not a real chain, and why it is acceptable here.
- **basefee = 0:** no burn, pre-1559 economics.
- **prevrandao = parent hash:** why not the current block's hash.
- **Changed tests:** list each, with the one-line justification (it is also in code comments).

## 7. Known gaps / what does not work

> Be specific. E.g. "topic filtering unimplemented; here's where I got stuck and what I think the fix is."

## 8. How to run and verify

```bash
cargo test --workspace            # N tests
cargo run --release -- --dev
./demo/demo.sh                    # deploys Counter.sol, calls it, shows logs and a revert
```

Demo output:

```text
(paste)
```
