## How it works

### Proof of work

A block header is RLP-encoded and hashed with keccak256. The miner tries nonce
after nonce until the hash starts with enough zero bits. There is no shortcut —
that search *is* the work.

Verification is a single hash. That asymmetry is the whole trick: a block that
cost millions of attempts to find takes microseconds to check, so any node can
reject a miner that lies about its work. `pow::verify` is the half every node
runs, and `every_block_is_a_valid_proof_of_work_linked_to_its_parent` checks it
across a freshly mined chain.

### Difficulty retargeting

Difficulty is a count of required leading zero bits, so one step is a *doubling*.
Breeja only adjusts when the last block's time was off by more than that factor
of two; otherwise the difficulty would oscillate forever around a target it can
never land on.

This is precisely why real Ethereum does not count zero bits. It adjusts a
256-bit difficulty *number* in steps of about 1/2048, so it tracks its target
closely instead of bracketing it. Watching Breeja settle at ±1 bit is a good way
to see why that resolution matters.

Expect wide variation in block times even at a settled difficulty — proof of work
is a memoryless search, so a block taking 4x the average is unremarkable.

### Execution

Transactions run in REVM. Breeja doesn't hand the EVM its state; the EVM *asks*
for it. A contract can touch any account or storage slot, so nobody knows in
advance what it will need. `BreejaDb` (`crates/node/src/evm.rs`) wraps a snapshot
of the world (accounts, code by hash, storage) and answers REVM's `Database`
questions as they come. When a transaction finishes, `DatabaseCommit` writes back
every account REVM marked as touched.

REVM does the whole state transition:
- checks the nonce and balance
- charges the sender for the full gas limit
- runs the code
- refunds unused gas
- bumps the nonce
- pays the fee to the block's beneficiary

Breeja adds only the block reward afterwards, which is still the only way new ETH
is created.

A transaction can fail in two very different ways. If REVM refuses it outright
(wrong nonce, can't afford its gas), it's invalid and skipped. If it runs and then
reverts or runs out of gas, it's valid: it's mined with `status: 0`, the sender
still pays for the gas it burned, and its nonce is still used.

Blocks fill by the gas their transactions actually use. Each transaction's gas
limit has to fit in whatever the block has left. The state root commits to every
account's balance and nonce, its code hash, and a root of its storage. Each block
also carries a bloom filter over its logs, in the hashed header.

The EVM follows Shanghai rules (so modern Solidity runs) with a basefee of zero
(so nothing is burned), on top of Frontier-style proof of work.

### Signatures

A transaction never states who sent it. The sender's address is *recovered* from
the signature, in `pool.rs`. That is why transactions cannot be forged: producing
a signature that recovers to someone else's address means finding their private
key.

### Concurrency

The miner owns a thread; the RPC server runs on Tokio. Both hold a `Chain`, which
wraps the state in an `RwLock`. The miner writes account state and blocks; the RPC
server only appends to the mempool.

The important detail is *when the lock is held*. Mining can take many seconds, so
the miner snapshots state, releases the lock, hashes, and re-acquires the lock
only for the instant it takes to commit. The RPC server never blocks on mining.

The miner also abandons a block in progress when a new transaction arrives, rather
than making that transaction wait out the current search. Real miners do the same,
for the same reason.

`eth_call` and `eth_estimateGas` follow the same rule. They copy the state they need
under the read lock, release it, and execute against the copy through REVM's
read-only `DatabaseRef`, so nothing they do can be committed. A gas estimate is
about twenty executions, and it never blocks the miner or a transaction being
submitted.
