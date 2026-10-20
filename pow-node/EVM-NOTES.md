# EVM-NOTES: Giving Breeja an EVM

**Author:** Akanimoh12 · **Branch:** `feat/evm` · **REVM:** 43.0.3 · **Spec:** SHANGHAI

## 1. Baseline (M0)

Before any change, `cargo test --workspace` passed **48** tests:

| Crate | Tests |
| --- | --- |
| `node` unit | 13 |
| `node` integration (`tests/mining.rs`) | 8 |
| `primitives` unit | 14 |
| `rpc` integration (`tests/json_rpc.rs`) | 13 |

A few blocks from `cargo run --release -- --dev`, with difficulty climbing from 3 bits
toward the 2-second target:

```text
sealed block number=1  txs=0 difficulty=3  attempts=10      took=0.00s
sealed block number=10 txs=0 difficulty=12 attempts=9317    took=0.01s
sealed block number=18 txs=0 difficulty=20 attempts=592039  took=0.41s
sealed block number=19 txs=0 difficulty=21 attempts=5690389 took=3.98s
sealed block number=20 txs=0 difficulty=21 attempts=934603  took=0.65s
```

At the end: **94** tests (node unit 29, `contracts.rs` 8, `mining.rs` 8, primitives 26,
rpc unit 2, `json_rpc.rs` 21). `cargo clippy --workspace --all-targets -- -D warnings` is clean,
and `cargo build --release` succeeds.

## 2. Part 0 answers

### Q1. The three mempool rules that assumed no EVM

All three were in `decode_and_check` in `pool.rs`:

- **Line 65, `TxKind::Create` is rejected.** I **removed** it. A transaction with no `to`
  is a deployment, and that is now a real thing. `to` became `Option<Address>` all the
  way through to the receipt.
- **Line 72, non-empty calldata is rejected.** I **removed** it. Calldata is how you call a
  contract, and it is now carried through as `input`.
- **Line 81, `gas < TRANSFER_GAS`.** This is the one that doesn't mention the EVM. It
  quietly assumes every transaction costs 21,000. I **relaxed** it into a real
  intrinsic-gas floor, `intrinsic_gas()` in `primitives::node`:
  - 21,000 base
  - 4 gas per zero byte and 16 per non-zero byte of calldata (EIP-2028)
  - +32,000 for a deployment
  - +2 per 32-byte word of initcode (EIP-3860, which is Shanghai)

  Deploying the hand-assembled test contract with 21,000 gas is now rejected with
  "below this transaction's intrinsic cost (53298)".

I also **added** a rule: reject `gas > BLOCK_GAS_LIMIT`. Before, every transaction was
21,000, so this couldn't happen. Now a transaction with a bigger limit could never fit in
any block, and it would sit in the mempool forever.

### Q2. Skip vs. include on failure

The old comment said failures are skipped because Breeja validates everything before it
reaches the mempool, so the only failures left are races, and races are better dropped.
That was true while a transaction could only fail by being invalid. With an EVM, a
perfectly valid transaction can `REVERT` or run out of gas halfway through. Real Ethereum
mines it anyway, charges the gas, advances the nonce, and records `status: 0`. If it
didn't, anyone could make every miner run expensive code for free.

So there are now two outcomes, and REVM's return type separates them for me:

- `Err(EVMError::Transaction(_))` means *invalid* (wrong nonce, can't afford
  `gas × price + value`, gas over the block limit). I still **skip** these.
- `Ok(result)` where the result isn't a success means *valid but failed*. I **include**
  these with `status: 0`.

`a_reverting_transaction_is_mined_charged_and_advances_the_nonce` pins this down.

### Q3. Reserving `gas_limit × price` but charging `TRANSFER_GAS × price`

The mempool has to make sure the sender can pay for the *worst case*, before anyone knows
how much the transaction will actually use. Ethereum reserves the whole gas limit up
front and refunds what isn't burned. Before the EVM, the burn was always exactly 21,000,
so the gap between the reservation and the charge was simply never spent. Now the burn
varies, and the difference is a real **refund**: REVM charges `gas_limit × price` up
front and gives back `(gas_limit − gas_used) × price` at the end. I kept the mempool's
reservation as it was, because it's the same check REVM does at execution
(`LackOfFundForMaxFee`).

### Q4. What per-block snapshots cost once accounts have storage

`state_history` keeps a full copy of the world after every block, which now means every
contract's code map and every storage slot. Memory grows as *number of blocks × total
state size*, even when a block touches one slot. It's fine for a teaching chain with a
handful of contracts, but it is the first thing that would fall over. Real nodes keep
one trie and share every unchanged node between versions, or store per-block diffs and
rebuild history from them. A cheap step in that direction would be a persistent map
(like `im::HashMap`) so clones share structure.

## 3. What I built

### M1: State that knows about code and storage

I kept `Account` as Ethereum's four fields, because it is RLP-encoded into the state
root, and added a `WorldState` around it:

- `accounts`
- `code: HashMap<B256, Bytes>`, keyed by code hash, so identical contracts are stored once
- `storage: HashMap<Address, HashMap<U256, U256>>`

Writing zero to a slot deletes it, so two states with the same contents always compare
and hash the same. `BlockState`, the history, the miner's snapshot and `commit_block` all
moved from a bare account map to `WorldState`.

`BreejaDb` in `crates/node/src/evm.rs` wraps a snapshot of the world (never the live
`BlockState` behind the `RwLock`) and implements REVM's `Database` and `DatabaseCommit`.
I started from Appendix A, then compared it against REVM's own `CacheDB::commit_account`
and changed three things:

1. **Storage is cleared when an account is newly created** (`is_created()`), as `CacheDB`
   does.
2. **Code is stored with `Bytecode::original_bytes()`.** REVM pads analysed legacy code
   internally, and that padding would otherwise have leaked into `eth_getCode`.
3. **(Added in M6) A touched account that ends up empty is deleted (EIP-161).**

### M2: Transfers through REVM, with the same results

I deleted `apply_transfer` and wrote `execute_transaction`. It builds `BlockEnv` from a
`BlockContext` that the miner now computes *before* executing, because contracts can read
the timestamp. It runs `transact`, drops the EVM, then commits. I removed the manual fee
credit and the manual nonce bump, since REVM does both. I kept the block reward, because
REVM knows nothing about it.

The parity test, `a_transfer_through_revm_costs_exactly_21000_gas`, checks four things:

- exactly 21,000 gas
- the beneficiary gets exactly `21,000 × 1 gwei`
- the recipient gets exactly the value
- the nonce goes up by exactly one

The supply-conservation canary passed without touching its assertion.

### M3: Contracts

- **Mempool:** the rule changes from Q1.
- **Execution:** REVM gets `TxKind::Create` when there's no `to`, and the calldata.
- **Receipts:** they got a real `contract_address` (from `created_address()`), real
  `logs`, and a real `status`.
- **Logs:** I added a `Log` type with its block and transaction position. `logIndex`
  counts across the whole block, and the block hash is stamped in after sealing, the
  same way it is for receipts.
- **Measured costs:** the hand-assembled test contract costs exactly **55,522** gas to
  deploy and **43,487** to call. These match what the REVM-backed anvil charges for the
  same bytecode.

### M4: `eth_call` and `eth_estimateGas`

I implemented REVM's read-only `DatabaseRef` for `BreejaDb`, and `simulate()` runs through
`WrapDatabaseRef(&db)`. Writes stay in REVM's journal and are dropped, so nothing can
reach the snapshot. One snapshot then serves every probe of a gas estimate without being
copied again. `Chain::simulation_env` copies the state at the requested block while
holding the read lock, then releases it, so a ~20-probe estimate never blocks the miner.
Block tags work (`earliest`, a number, `latest`).

The estimate works in three steps:

1. **Run once with the full cap.** If it fails there, return the failure, because more
   gas would not help.
2. **Try exactly the `gas_used` that run reported.** It usually succeeds.
3. **If not, binary search** between that and the cap. Every number returned has actually
   been run and succeeded.

It searches on *success*, not on `gas_used`, because refunds make the reported number
smaller than what the call needs while it runs. A plain transfer comes out at exactly
`0x5208`. A contract call sent with exactly the estimate succeeds, and one gas less halts
(`an_estimate_is_enough_and_one_less_is_not`).

### M5: The methods contracts need

- **New methods:** `eth_getCode`, `eth_getStorageAt` (always a 32-byte word),
  `eth_gasPrice` and `eth_getLogs`.
- **`eth_getLogs` filtering:** `fromBlock`/`toBlock` or `blockHash`, one or many
  addresses, and topics with `null` wildcards and OR-lists. I followed geth's rule that a
  filter with N topic positions never matches a log with fewer than N topics, so `[null]`
  means "has at least one topic".

Getting real tools to work turned up two broken response shapes (see section 5). After
fixing them, `cast send` works without `--gas-price`, and so do `cast call`, `cast code`,
`cast storage`, `cast logs`, `cast block` and `cast receipt`.

### M6: Holding together

- **Blocks fill by gas used.** The 149-transaction cap is gone. Each transaction's
  *limit* must fit in the gas the block has left (Ethereum's rule), and blocks fill by
  what transactions actually use. On a live node, four 1,000,000-gas calls sent together
  landed three in one block (3,000,000) and one in the next.
- **`logsBloom` is real.** I wrote it by hand following the Yellow Paper, and it matches
  alloy's implementation bit for bit. Receipts carry their own bloom, and blocks carry the
  OR of all their logs. I put the block bloom in the hashed header, as Ethereum does, so
  the proof of work covers it.
- **The state root commits to code and storage.** It goes through `code_hash` and a
  storage root that is recomputed from the slots, not read from the cached field. So
  even a write that forgot to refresh `storage_root` still changes the root.

### Demo

`demo/demo.sh` deploys `demo/Counter.sol` with `forge create` and checks every step with
`cast`. The output is in section 8.

## 4. The learning objectives, answered from my code

1. **Why a `Database`.** REVM can't know in advance which accounts, code or slots a
   contract will touch, so it asks for them during execution through `basic`,
   `code_by_hash`, `storage` and `block_hash`. `DatabaseCommit::commit` is the other
   direction: REVM hands back every account it loaded, and only the ones marked touched
   are written. `Database` alone can only answer questions. It has no way to receive the
   changes.
2. **What an account is now.** It's still four fields, but `code_hash` and `storage_root`
   finally commit to something real. Without them in the state root, two chains with
   different contract storage would have the same root
   (`states_differing_in_one_storage_slot_have_different_roots`).
3. **Where gas comes from.** It's the intrinsic cost (base + calldata + creation) plus
   whatever the opcodes charge while running. The same call to the test contract cost
   43,487 the first time and 23,587 once slot 0 already held 42. That's why
   `eth_estimateGas` has to execute, and why it's a search and not a formula.
4. **What a reverted transaction is.** It's valid, so it's mined and paid for, but its
   effects (except the fee and the nonce) are undone. `status` is the only way to tell it
   apart from a success once it's in a block. On the live node a reverted call paid
   21,514 gas and still used its nonce.
5. **`eth_call` vs. `eth_sendRawTransaction`.** It's the same EVM with the same code. The
   only difference is that `simulate()` never calls `commit`.
6. **Who pays the miner.** REVM pays the fee to `BlockEnv.beneficiary`, and Breeja pays
   the block reward. Keeping the old manual fee credit would have paid every fee twice.
   The parity test and the supply-conservation test would both catch that.

## 5. What broke, and how I found it

| Symptom | Caught by | Cause | Fix |
| --- | --- | --- | --- |
| `eth_getCode` could have returned extra trailing bytes | Reading `revm-bytecode` source before writing `commit` | Analysed legacy bytecode is padded internally | Store `original_bytes()` |
| Redeploying at an address could inherit old storage | Comparing my `commit` with REVM's `CacheDB::commit_account` | Appendix A doesn't clear storage on `is_created()` | Clear it, like `CacheDB` |
| My live revert check returned `status 0x1` and 21,000 gas | Manual check on the dev node in M3 | My script called the address for nonce 1, but the contract was deployed at nonce 2: an empty account, so a plain transfer | Computed the address with `cast compute-address`. The node was right |
| `cast call` on a contract that returns nothing failed with `Method not found` | Live `cast` session in M4 | cast calls `eth_getCode` when the output is empty, and that method didn't exist yet | Implemented in M5 |
| cast couldn't parse receipts | Reading the receipt JSON | Field was spelled `cummulativeGasUsed` | Renamed to `cumulativeGasUsed` (in Rust too) |
| `cast send` logged `missing field sha3Uncles` | Live `cast send` in M5 | Block JSON lacked `sha3Uncles`, `mixHash`, `logsBloom` | Added them with honest empty values, plus a test that parses blocks, transactions, receipts and logs with alloy's own types |
| A 0-ETH transfer to a fresh address left an empty account in the state root | Comparing my `commit` with REVM's `State` in M6 | EIP-161: touched empty accounts must be deleted | Delete them. I checked the test fails without the fix |
| cast showed raw hex instead of the revert reason | Running the demo | The error message didn't decode `Error(string)` | Put `execution reverted: <reason>` in the message like geth (and describe `Panic` codes) |
| One of my bloom assertions never ran | Re-reading the test | Both transactions always landed in the same block | Replaced it with a check over every other block |

Pitfalls 1–3 (double fee, basefee burn, double nonce) never showed up as bugs, because I
wrote the parity test before the REVM code and kept the supply test's assertion fixed.

## 6. Decisions and compromises

- **Frontier consensus, Shanghai execution.** The blocks are proof of work with 5 ETH
  rewards and the "pi million" gas limit, but the EVM follows Shanghai rules so modern
  Solidity (`PUSH0` and so on) runs. That's not a chain that ever existed. Shanghai is the
  newest fork that doesn't need blob-gas fields in `BlockEnv`. `demo/foundry.toml` pins
  `evm_version = "shanghai"` because solc's default emits Cancun opcodes like `MCOPY`.
- **`basefee = 0`.** This keeps Breeja pre-1559: one fixed price, nothing burned, so the
  supply-conservation test still holds exactly.
- **`prevrandao = parent hash`.** The current block's hash doesn't exist when the EVM runs,
  because execution happens before mining and the hash depends on the state root that
  execution produces.
- **Empty code hash.** Inside Breeja, "no code" stays `B256::ZERO`, so genesis and EOA
  roots didn't change. `evm_code_hash()` translates it to `KECCAK_EMPTY` at the REVM
  boundary, which is the only place the difference matters.
- **Unknown address → `None`** from `Database::basic`, so REVM treats it as non-existent.
  That decides the 25,000-gas new-account charge.
- **`eth_call` skips both the nonce and the balance check.** A call request has no nonce,
  and reading a contract shouldn't need funds. The balance check needed REVM's
  `optional_balance_check` feature. With it off, REVM tops the caller up to the call's
  value, which is how anvil behaves. **`eth_estimateGas` keeps the balance check**: an
  estimate for something the sender could never send should be an error. Both default
  the gas price to 0.
- **Blocks stop at the first transaction that doesn't fit.** This keeps the mempool
  first-come first-served and each sender in nonce order. It can leave a block under-full
  when a small transaction is waiting behind a big one.
- **The logs bloom is in the hashed header.** That changed every block hash compared with
  before. No test pinned a hash, and the proof-of-work test re-derives hashes from headers.

### Changed tests (the reason is also in a comment beside each)

- `executor.rs` unit tests: `apply_transfer` no longer exists, so they go through REVM.
  Every assertion is unchanged.
- `mining.rs::a_signed_transfer_is_mined_and_moves_the_eth`: `tx.to == bob` became
  `Some(bob)`.
- `params.rs`: the `needs_evm()` test was replaced by a test of reading calldata, because
  with an EVM every request can be answered.
- `json_rpc.rs`:
  - The `eth_call` test expected a "no EVM" error for calldata. Calldata to an address
    with no code now returns `0x`.
  - The estimate test expected deployments to be refused. They're now estimated.
  - The method-not-found test used `eth_gasPrice`, which now exists, so it uses
    `eth_feeHistory`.
- `node.rs` state-root test: wraps its accounts in `WorldState`. `pow.rs`'s test header
  gained the `logs_bloom` field.

## 7. Known gaps and what doesn't work

- **The receipts root doesn't commit to receipts.** `receipts_root` is hashed from the
  transaction hashes, the same input as `transactions_root`. That's how the code already
  was before I started, but with contracts it matters: two blocks could disagree on a
  transaction's `status` or logs and still have the same receipts root. The fix is to
  hash each receipt's encoding (status, cumulative gas, bloom, logs) instead. I left it,
  because it changes block hashes again and nothing in the brief tested it.
- **No tries anywhere.** The state, storage, transactions and receipts roots are flat
  hashes of sorted data. They commit to the contents but can't produce inclusion proofs,
  so there's no `eth_getProof`.
- **Invalid transactions are never purged.** A transaction that stops being valid stays
  in the mempool and is retried, with a warning, every block. Only mined transactions are
  removed.
- **The estimate is a heuristic.** A contract that reads `GAS` can behave differently on
  each probe. Also, when a caller does pass a `gasPrice`, I don't lower the cap to what
  their balance can afford, like geth does. They just get an "insufficient funds" error.
- **`SELFDESTRUCT`** is handled in `commit` and unit-tested with hand-built REVM changes,
  but no test runs a real self-destructing contract.
- **`eth_call` against `pending`** runs against `latest`, and the simulated block's
  timestamp is the parent's + 1. The real next timestamp isn't known until it's mined.
- **`eth_getLogs` has no range limit**, so a very long chain would be slow to scan.
- **Memory:** see Q4. The full history clone is the scaling limit.
- **Precompiles** come from REVM and aren't tested here. Only legacy transactions are
  accepted.

## 8. How to run and verify

```bash
cargo test --workspace            # 94 tests
cargo clippy --workspace --all-targets -- -D warnings
cargo run --release -- --dev
./demo/demo.sh                    # uses the node above, or starts its own
```

Demo output against a fresh `--dev` node:

```text
== The chain
   ✓ chain id: 1
   ✓ gas price (wei): 1000000000
== Deploy Counter.sol (compiled for Shanghai)
   ✓ deployment status: 0x1
   ✓ deployed to 0x5FbDB2315678afecb367f032d93F642f64180aa3 in block 17, 212776 gas
   ✓ eth_getCode matches the compiled runtime (742 bytes)
== Read with eth_call
   ✓ number(): 0
== Estimate, then send increment()
   ✓ eth_estimateGas: 45231
   ✓ increment() sent with exactly the estimate: 0x1
   ✓ logs emitted: 1
== State after the transaction
   ✓ number(): 1
   ✓ eth_getStorageAt slot 0: 0x0000000000000000000000000000000000000000000000000000000000000001
== Find the event with eth_getLogs
   ✓ Incremented events in block 19: 1
   ✓ topic 0 (event signature): 0x38ac789ed44572701765277c4d0970f2db1c1a571ed39e84358095ae4eaa5420
   ✓ topic 1 (indexed sender): 0x000000000000000000000000f39fd6e51aad88f6f4ce6ab8827279cfffb92266
   ✓ data (newValue): 1
   ✓ events filtered by indexed sender: 1
== eth_call runs the code but keeps nothing
   ✓ number() after a simulated increment(): 1
== setNumber(42)
   ✓ number(): 42
== A revert, simulated: eth_call reports the reason
   ✓ revert reason decoded: Counter: nope
== A revert, mined: included, charged, nonce advanced
   ✓ receipt status: 0x0
   ✓ gas still paid: 21514
   ✓ sender nonce: 4
   ✓ listed in block 22
   ✓ number() untouched by the revert: 42
All checks passed.
```
