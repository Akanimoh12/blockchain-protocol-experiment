use std::collections::HashMap;

use alloy::primitives::{Address, B256, Bloom, Bytes, U256, keccak256};
use alloy_rlp::{Encodable, RlpEncodable};
use serde::{Deserialize, Serialize};

use crate::constants::{
    BLOCK_GAS_LIMIT, EXTRA_DATA, GAS_PRICE, MAX_DIFFICULTY_LEADING_ZEROS,
    MIN_DIFFICULTY_LEADING_ZEROS, TRANSFER_GAS,
};

/// Ethereum's four-field account record.
///
/// `code_hash` and `storage_root` are *commitments*: the code and storage they
/// commit to live in [`WorldState`], not here, because this struct is what gets
/// RLP-encoded into the state root. For an account with no code or no storage
/// both are `B256::ZERO` — Breeja's convention, where Ethereum would use
/// `keccak256("")` and the empty-trie root. The EVM boundary translates.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, RlpEncodable)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub nonce: u64,
    pub balance: U256,
    pub storage_root: B256,
    pub code_hash: B256,
}

/// Everything an EVM can read or write: accounts, the code they run, and their
/// storage.
///
/// Code is keyed by its hash rather than by address, so a thousand copies of the
/// same contract are stored once. That is what real nodes do, and it is the
/// lookup REVM's `code_by_hash` expects to exist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorldState {
    pub accounts: HashMap<Address, Account>,
    /// Runtime bytecode by `keccak256(code)`.
    pub code: HashMap<B256, Bytes>,
    /// Storage slots per account. A slot holding zero is *absent*, never stored
    /// as zero, so two states with the same contents always compare and hash
    /// equal.
    pub storage: HashMap<Address, HashMap<U256, U256>>,
}

/// A sealed block.
///
/// Everything above `hash` is the *header*: the bytes that are hashed to produce
/// `hash`. `nonce` and `hash` are the proof of work — the miner searches for a
/// `nonce` that makes `hash` small enough to satisfy `difficulty`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    pub number: u64,
    pub parent_hash: B256,
    pub state_root: B256,
    pub transaction_root: B256,
    pub receipts_root: B256,
    /// Bloom filter over every log in the block, so a client can skip blocks
    /// that cannot contain the event it is looking for.
    pub logs_bloom: Bloom,
    /// Number of leading zero bits `hash` must have for this block to be valid.
    pub difficulty: u8,
    /// Sum of every ancestor's difficulty plus this block's. The heaviest chain
    /// wins, which is what makes proof of work a consensus rule and not just a
    /// puzzle.
    pub total_difficulty: u64,
    pub timestamp: u64,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub miner: Address,
    pub extra_data: Bytes,
    /// The proof-of-work solution.
    pub nonce: u64,
    /// `keccak256(rlp(header))`, which is only valid once `nonce` has been found.
    pub hash: B256,
    /// Hashes of the transactions this block executed, in execution order.
    pub transactions: Vec<B256>,
}

/// The part of a [`Block`] that the proof of work commits to.
///
/// This is a separate struct so that the hashed bytes are unambiguous: RLP
/// encode exactly these fields, in exactly this order, and hash the result.
/// Mining is the search for a `nonce` that makes that hash small enough.
#[derive(Debug, Clone, RlpEncodable)]
pub struct BlockHeader {
    pub number: u64,
    pub parent_hash: B256,
    pub state_root: B256,
    pub transaction_root: B256,
    pub receipts_root: B256,
    /// Hashed like the rest of the header, as on Ethereum: a miner cannot
    /// advertise logs its block does not contain.
    pub logs_bloom: Bloom,
    pub difficulty: u8,
    pub timestamp: u64,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub miner: Address,
    pub extra_data: Bytes,
    pub nonce: u64,
}

/// A transaction that has been included in a block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    pub hash: B256,
    pub block_hash: B256,
    pub block_number: u64,
    pub transaction_index: u64,
    pub chain_id: u64,
    pub from: Address,
    /// `None` for a contract deployment, which has no recipient yet — the
    /// address it creates is in the receipt.
    pub to: Option<Address>,
    pub value: U256,
    pub nonce: u64,
    pub gas: u64,
    pub gas_price: u128,
    pub input: Bytes,
    pub v: u64,
    pub r: U256,
    pub s: U256,
}

/// The receipt proving what a transaction did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionReceipt {
    pub transaction_hash: B256,
    pub transaction_index: u64,
    pub block_hash: B256,
    pub block_number: u64,
    pub from: Address,
    pub to: Option<Address>,
    pub gas_used: u64,
    /// Gas used by this transaction and every one before it in the block.
    pub cumulative_gas_used: u64,
    pub effective_gas_price: u128,
    /// The address a deployment created; `None` for everything else, including
    /// a deployment that reverted.
    pub contract_address: Option<Address>,
    pub logs: Vec<Log>,
    /// `1` if execution succeeded, `0` if it reverted or halted.
    ///
    /// A failed transaction is still mined and still pays for the gas it burned:
    /// the miner did the work of running it either way. This field is the only
    /// way to tell the two apart once the transaction is in a block.
    pub status: u8,
}

/// An event emitted by a contract's `LOG0`–`LOG4` opcodes.
///
/// The address, topics and data are what the contract emitted. The rest locates
/// the log on chain, which `eth_getLogs` and receipts both report.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Log {
    /// The contract that emitted it.
    pub address: Address,
    /// Up to four indexed values; the first is usually the event signature hash.
    pub topics: Vec<B256>,
    pub data: Bytes,
    pub block_hash: B256,
    pub block_number: u64,
    pub transaction_hash: B256,
    pub transaction_index: u64,
    /// Position among *all* logs in the block, not just this transaction's.
    pub log_index: u64,
}

/// A transaction that has been accepted into the mempool but not yet mined.
#[derive(Debug, Clone)]
pub struct PendingTransaction {
    pub hash: B256,
    pub from: Address,
    /// `None` deploys a contract.
    pub to: Option<Address>,
    pub value: U256,
    pub nonce: u64,
    pub gas: u64,
    pub gas_price: u128,
    /// Calldata for a call, or initcode for a deployment.
    pub input: Bytes,
    pub chain_id: u64,
    pub v: u64,
    pub r: U256,
    pub s: U256,
}

/// The gas a transaction costs before any of its code runs, under Shanghai
/// rules.
///
/// Every transaction pays 21,000. Calldata is charged per byte — 4 for a zero
/// byte, 16 for anything else (EIP-2028) — because every node has to store and
/// relay it. A deployment pays 32,000 more to create an account, plus 2 per
/// 32-byte word of initcode (EIP-3860) for the analysis it forces.
///
/// A transaction whose gas limit is below this can never execute, so the
/// mempool rejects it outright.
pub fn intrinsic_gas(input: &[u8], is_create: bool) -> u64 {
    let zeros = input.iter().filter(|byte| **byte == 0).count() as u64;
    let non_zeros = input.len() as u64 - zeros;
    let mut gas = TRANSFER_GAS + zeros * 4 + non_zeros * 16;
    if is_create {
        gas += 32_000 + 2 * (input.len() as u64).div_ceil(32);
    }
    gas
}

/// The accounts a chain starts with, before any block has been mined.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BreejaGenesis {
    pub accounts: Vec<(Address, U256)>,
}

/// Everything the node knows: the current account state, the chain of blocks,
/// the transactions and receipts in them, and the mempool of transactions
/// waiting to be mined.
#[derive(Debug, Clone, Default)]
pub struct BlockState {
    /// Accounts, code and storage as of the chain head.
    world: WorldState,
    /// Blocks by number; `blocks[n]` is the block with `number == n`.
    blocks: Vec<Block>,
    /// World state as it was *after* each block, so historical balance, nonce,
    /// code and storage queries can be answered honestly. A real node would
    /// reconstruct this from a trie; Breeja keeps whole snapshots, which is only
    /// affordable because a teaching chain has a handful of accounts. Once
    /// contracts hold storage, every block copies every slot — see
    /// `EVM-NOTES.md`, Part 0 question 4.
    state_history: Vec<WorldState>,
    /// Block number for a given block hash.
    block_index: HashMap<B256, u64>,
    transactions: HashMap<B256, Transaction>,
    receipts: HashMap<B256, TransactionReceipt>,
    /// Validated transactions waiting to be included in a block, oldest first.
    mempool: Vec<PendingTransaction>,
}

impl Account {
    pub fn new(balance: U256) -> Self {
        Self {
            balance,
            ..Default::default()
        }
    }

    /// Consumes this account's current nonce and advances it, the way sending a
    /// transaction does.
    pub fn increase_nonce(&mut self) -> u64 {
        let used_nonce = self.nonce;
        self.nonce += 1;
        used_nonce
    }

    /// Debits `amount`, failing if the account cannot cover it.
    pub fn debit(&mut self, amount: U256) -> Result<U256, anyhow::Error> {
        match self.balance.checked_sub(amount) {
            Some(remaining) => {
                self.balance = remaining;
                Ok(self.balance)
            }
            None => anyhow::bail!(
                "insufficient balance: have {}, need {}",
                self.balance,
                amount
            ),
        }
    }

    /// Credits `amount`.
    pub fn credit(&mut self, amount: U256) -> Result<U256, anyhow::Error> {
        match self.balance.checked_add(amount) {
            Some(total) => {
                self.balance = total;
                Ok(self.balance)
            }
            None => anyhow::bail!("balance overflow crediting {}", amount),
        }
    }
}

impl WorldState {
    /// Builds a world of plain accounts with no code and no storage, which is
    /// what genesis produces.
    pub fn from_accounts(accounts: HashMap<Address, Account>) -> Self {
        Self {
            accounts,
            ..Default::default()
        }
    }

    /// The account at `address`, empty if nobody has ever touched it.
    pub fn account(&self, address: &Address) -> Account {
        self.accounts.get(address).cloned().unwrap_or_default()
    }

    /// Mutable access to an account, creating it empty if it does not exist.
    pub fn account_mut(&mut self, address: Address) -> &mut Account {
        self.accounts.entry(address).or_default()
    }

    /// The runtime code at `address`, empty for an account with no code.
    pub fn code_of(&self, address: &Address) -> Bytes {
        self.accounts
            .get(address)
            .filter(|account| account.code_hash != B256::ZERO)
            .and_then(|account| self.code.get(&account.code_hash))
            .cloned()
            .unwrap_or_default()
    }

    /// One storage slot of `address`. An unwritten slot reads as zero, as in the
    /// EVM.
    pub fn storage_at(&self, address: &Address, slot: U256) -> U256 {
        self.storage
            .get(address)
            .and_then(|slots| slots.get(&slot))
            .copied()
            .unwrap_or_default()
    }

    /// Installs runtime code at `address` and points its `code_hash` at it.
    ///
    /// Empty code means "no code", which Breeja records as a zero hash.
    pub fn set_code(&mut self, address: Address, code: Bytes) {
        if code.is_empty() {
            self.account_mut(address).code_hash = B256::ZERO;
            return;
        }
        let hash = keccak256(&code);
        self.code.insert(hash, code);
        self.account_mut(address).code_hash = hash;
    }

    /// Writes one storage slot. Writing zero deletes the slot, so that the state
    /// root does not depend on whether a slot was ever touched.
    ///
    /// This does not refresh `storage_root`; call
    /// [`WorldState::refresh_storage_root`] once a batch of writes is done.
    pub fn set_storage(&mut self, address: Address, slot: U256, value: U256) {
        if value.is_zero() {
            if let Some(slots) = self.storage.get_mut(&address) {
                slots.remove(&slot);
                if slots.is_empty() {
                    self.storage.remove(&address);
                }
            }
        } else {
            self.storage.entry(address).or_default().insert(slot, value);
        }
    }

    /// Recomputes the `storage_root` commitment for `address` from its slots.
    pub fn refresh_storage_root(&mut self, address: Address) {
        let root = self
            .storage
            .get(&address)
            .map(storage_root_of)
            .unwrap_or(B256::ZERO);
        if let Some(account) = self.accounts.get_mut(&address) {
            account.storage_root = root;
        }
    }

    /// Deletes an account and its storage, as `SELFDESTRUCT` does.
    ///
    /// The code itself stays in the code map: other accounts may share it, and
    /// code is addressed by hash precisely so it never needs reference counting.
    pub fn remove_account(&mut self, address: &Address) {
        self.accounts.remove(address);
        self.storage.remove(address);
    }
}

/// Commits to one account's storage in a single hash.
///
/// Slots are sorted first so the root never depends on `HashMap` order, and an
/// empty storage commits to zero. Ethereum uses a Merkle-Patricia trie here; this
/// commits to the same contents but cannot produce inclusion proofs.
pub fn storage_root_of(slots: &HashMap<U256, U256>) -> B256 {
    if slots.is_empty() {
        return B256::ZERO;
    }
    let mut entries: Vec<(&U256, &U256)> = slots.iter().collect();
    entries.sort_by_key(|(slot, _)| **slot);

    let mut buf = Vec::with_capacity(entries.len() * 64);
    for (slot, value) in entries {
        buf.extend_from_slice(&slot.to_be_bytes::<32>());
        buf.extend_from_slice(&value.to_be_bytes::<32>());
    }
    keccak256(buf)
}

impl BlockHeader {
    /// `keccak256(rlp(header))` — the block hash, and the value proof of work
    /// has to drive below the difficulty target.
    pub fn hash(&self) -> B256 {
        let mut buf = Vec::new();
        self.encode(&mut buf);
        keccak256(buf)
    }
}

impl Block {
    /// Rebuilds the header that was hashed to produce [`Block::hash`], so any
    /// node can re-derive the hash and check the proof of work for itself.
    pub fn header(&self) -> BlockHeader {
        BlockHeader {
            number: self.number,
            parent_hash: self.parent_hash,
            state_root: self.state_root,
            transaction_root: self.transaction_root,
            receipts_root: self.receipts_root,
            logs_bloom: self.logs_bloom,
            difficulty: self.difficulty,
            timestamp: self.timestamp,
            gas_limit: self.gas_limit,
            gas_used: self.gas_used,
            miner: self.miner,
            extra_data: self.extra_data.clone(),
            nonce: self.nonce,
        }
    }
}

/// Counts the leading zero bits of a hash. A hash with more leading zeros is a
/// smaller number, so "at least N leading zero bits" is the same rule as
/// "hash below 2^(256-N)" — just easier to read in a log line.
///
/// The return type is wider than the `u8` a difficulty is stored in because an
/// all-zero hash has 256 leading zero bits, which a `u8` cannot hold.
pub fn leading_zero_bits(hash: &B256) -> u16 {
    let mut bits = 0u16;
    for byte in hash.0.iter() {
        if *byte == 0 {
            bits += 8;
        } else {
            bits += byte.leading_zeros() as u16;
            break;
        }
    }
    bits
}

/// Whether `hash` satisfies a difficulty of `difficulty` leading zero bits.
pub fn meets_difficulty(hash: &B256, difficulty: u8) -> bool {
    leading_zero_bits(hash) >= difficulty as u16
}

/// Picks the difficulty for the next block.
///
/// This is Breeja's difficulty adjustment algorithm: if blocks are coming in too
/// fast, demand one more zero bit; if too slow, demand one fewer.
///
/// The catch is that one bit is a *doubling*. A difficulty of 22 bits and one of
/// 23 bits can easily straddle the target with neither landing on it, so adjusting
/// whenever the time is merely off would oscillate forever. Breeja therefore only
/// moves when the measured time is off by more than the 2x a single bit can
/// correct, which leaves block times inside a factor-of-two band around the
/// target.
///
/// This is exactly why real Ethereum does not count zero bits. It adjusts a
/// 256-bit difficulty *number* in steps of about 1/2048, so it can track a target
/// block time closely instead of bracketing it.
pub fn retarget_difficulty(current: u8, actual_ms: u64, target_ms: u64) -> u8 {
    let next = if actual_ms.saturating_mul(2) < target_ms {
        current.saturating_add(1)
    } else if actual_ms > target_ms.saturating_mul(2) {
        current.saturating_sub(1)
    } else {
        current
    };
    next.clamp(MIN_DIFFICULTY_LEADING_ZEROS, MAX_DIFFICULTY_LEADING_ZEROS)
}

/// Commits to the whole world — every account's balance and nonce, its code and
/// its storage — in a single hash.
///
/// Code is covered through `code_hash`. Storage is covered through each
/// account's storage root, which is recomputed here from the slots themselves
/// rather than read from the cached `storage_root` field: a write that forgot to
/// refresh the cache must not be able to produce a root that hides it. Without
/// this, two chains with different contract state could share a state root,
/// which would make the root worthless.
///
/// Addresses are sorted first so that two nodes holding the same state always
/// produce the same root, regardless of `HashMap` iteration order. Ethereum gets
/// that ordering for free from its trie.
pub fn state_root_of(world: &WorldState) -> B256 {
    let mut entries: Vec<(&Address, &Account)> = world.accounts.iter().collect();
    entries.sort_by_key(|(address, _)| **address);

    let mut buf = Vec::new();
    for (address, account) in entries {
        let committed = Account {
            storage_root: world
                .storage
                .get(address)
                .map(storage_root_of)
                .unwrap_or(B256::ZERO),
            ..account.clone()
        };
        buf.extend_from_slice(address.as_slice());
        committed.encode(&mut buf);
    }
    keccak256(buf)
}

/// The 2048-bit bloom filter over a set of logs, as receipts and block headers
/// carry it.
///
/// A bloom filter answers "might these logs mention X?" without reading them:
/// a client scanning for an event can skip every block whose bloom says no.
/// Each log's address and every topic is hashed, and three 11-bit slices of the
/// hash each switch on one bit. If any of X's three bits is off, X is definitely
/// absent; if all are on, it is *probably* present (Yellow Paper §4.3.1, `M3:2048`).
pub fn logs_bloom<'a>(logs: impl IntoIterator<Item = &'a Log>) -> Bloom {
    let mut bloom = [0u8; 256];
    for log in logs {
        accrue_bloom(&mut bloom, log.address.as_slice());
        for topic in &log.topics {
            accrue_bloom(&mut bloom, topic.as_slice());
        }
    }
    Bloom::from(bloom)
}

/// Sets the three bits `input` maps to. Bit 0 is the lowest bit of the *last*
/// byte, because the filter is one big-endian 2048-bit number.
fn accrue_bloom(bloom: &mut [u8; 256], input: &[u8]) {
    let hash = keccak256(input);
    for i in [0, 2, 4] {
        let bit = ((usize::from(hash[i]) << 8) | usize::from(hash[i + 1])) & 2047;
        bloom[255 - bit / 8] |= 1 << (bit % 8);
    }
}

/// Hashes an ordered list of hashes into a single root.
///
/// Ethereum uses a Merkle-Patricia trie here, which lets a light client prove
/// that one item is in the root without downloading the rest. Breeja just hashes
/// the concatenation: it still commits to the full contents, but it cannot
/// produce those proofs.
pub fn hash_list_root(items: &[B256]) -> B256 {
    if items.is_empty() {
        return B256::ZERO;
    }
    let mut buf = Vec::with_capacity(items.len() * 32);
    for item in items {
        buf.extend_from_slice(item.as_slice());
    }
    keccak256(buf)
}

impl BlockState {
    /// Builds the starting state from a genesis definition and seals the genesis
    /// block over it.
    pub fn new_with_gensis(breeja_gen: BreejaGenesis, difficulty: u8) -> Self {
        let mut accounts = HashMap::new();
        for (address, balance) in breeja_gen.accounts {
            accounts.insert(address, Account::new(balance));
        }

        let mut state = Self {
            world: WorldState::from_accounts(accounts),
            ..Default::default()
        };

        // The genesis block is the one block nobody mines: it has no parent, no
        // transactions, and its proof of work is accepted by definition.
        let header = BlockHeader {
            number: 0,
            parent_hash: B256::ZERO,
            state_root: state_root_of(&state.world),
            transaction_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: Bloom::ZERO,
            difficulty,
            timestamp: 0,
            gas_limit: BLOCK_GAS_LIMIT,
            gas_used: 0,
            miner: Address::ZERO,
            extra_data: Bytes::from_static(EXTRA_DATA.as_bytes()),
            nonce: 0,
        };
        let genesis = Block {
            hash: header.hash(),
            total_difficulty: difficulty as u64,
            transactions: Vec::new(),
            number: header.number,
            parent_hash: header.parent_hash,
            state_root: header.state_root,
            transaction_root: header.transaction_root,
            receipts_root: header.receipts_root,
            logs_bloom: header.logs_bloom,
            difficulty: header.difficulty,
            timestamp: header.timestamp,
            gas_limit: header.gas_limit,
            gas_used: header.gas_used,
            miner: header.miner,
            extra_data: header.extra_data.clone(),
            nonce: header.nonce,
        };

        state.block_index.insert(genesis.hash, 0);
        state.blocks.push(genesis);
        state.state_history.push(state.world.clone());
        state
    }

    /// Number of the chain head. Genesis is block 0, so a fresh chain is at 0.
    pub fn get_block_number(&self) -> u64 {
        self.blocks.len() as u64 - 1
    }

    pub fn head(&self) -> &Block {
        self.blocks
            .last()
            .expect("chain always contains the genesis block")
    }

    pub fn get_block(&self, number: u64) -> Option<&Block> {
        self.blocks.get(number as usize)
    }

    pub fn get_block_by_hash(&self, hash: &B256) -> Option<&Block> {
        self.block_index
            .get(hash)
            .and_then(|number| self.get_block(*number))
    }

    pub fn get_transaction(&self, hash: &B256) -> Option<&Transaction> {
        self.transactions.get(hash)
    }

    pub fn get_receipt(&self, hash: &B256) -> Option<&TransactionReceipt> {
        self.receipts.get(hash)
    }

    // account queries 

    /// Account at the chain head. An address nobody has ever funded reads as an
    /// empty account rather than an error, exactly as on Ethereum.
    pub fn get_account(&self, address: &Address) -> Account {
        self.world.account(address)
    }

    /// Account as it was after block `number`, or `None` if that block does not
    /// exist.
    pub fn get_account_at(&self, address: &Address, number: u64) -> Option<Account> {
        self.state_history
            .get(number as usize)
            .map(|snapshot| snapshot.account(address))
    }

    /// Runtime code at `address` as of block `number`: empty for an account with
    /// no code, `None` if that block does not exist.
    pub fn get_code_at(&self, address: &Address, number: u64) -> Option<Bytes> {
        self.state_history
            .get(number as usize)
            .map(|snapshot| snapshot.code_of(address))
    }

    /// One storage slot of `address` as of block `number`: zero if never written,
    /// `None` if that block does not exist.
    pub fn get_storage_at(&self, address: &Address, slot: U256, number: u64) -> Option<U256> {
        self.state_history
            .get(number as usize)
            .map(|snapshot| snapshot.storage_at(address, slot))
    }

    /// Hashes of the 256 blocks up to and including `number` — the window the
    /// `BLOCKHASH` opcode can see from the block after it.
    pub fn recent_block_hashes(&self, number: u64) -> HashMap<u64, B256> {
        let end = (number as usize + 1).min(self.blocks.len());
        let start = end.saturating_sub(256);
        self.blocks[start..end]
            .iter()
            .map(|block| (block.number, block.hash))
            .collect()
    }

    /// A copy of the whole world as it was after block `number`, for simulating
    /// against historical state. `None` if that block does not exist.
    pub fn world_at(&self, number: u64) -> Option<WorldState> {
        self.state_history.get(number as usize).cloned()
    }

    /// The nonce the sender's *next* transaction must carry: the mined nonce plus
    /// anything already sitting in the mempool for that sender.
    pub fn get_pending_nonce(&self, address: &Address) -> u64 {
        let mined = self.get_account(address).nonce;
        let queued = self.mempool.iter().filter(|tx| &tx.from == address).count() as u64;
        mined + queued
    }

    /// Balance minus everything the mempool has already committed to spending,
    /// so two transactions cannot each be validated against the same ETH.
    pub fn get_pending_balance(&self, address: &Address) -> U256 {
        let balance = self.get_account(address).balance;
        let committed: U256 = self
            .mempool
            .iter()
            .filter(|tx| &tx.from == address)
            .map(|tx| tx.value + U256::from(tx.gas) * U256::from(tx.gas_price))
            .fold(U256::ZERO, |acc, cost| acc + cost);
        balance.saturating_sub(committed)
    }

    /// Commits to the account set at the chain head in a single hash.
    pub fn state_root(&self) -> B256 {
        state_root_of(&self.world)
    }

    /// A copy of the world state at the head, for the miner (and later the EVM)
    /// to execute against without holding the state lock.
    pub fn world_snapshot(&self) -> WorldState {
        self.world.clone()
    }

    // mempool

    pub fn add_to_mempool(&mut self, tx: PendingTransaction) {
        self.mempool.push(tx);
    }

    pub fn mempool_len(&self) -> usize {
        self.mempool.len()
    }

    /// Whether this exact transaction is already queued, so resubmitting it is
    /// not counted twice.
    pub fn mempool_contains(&self, hash: &B256) -> bool {
        self.mempool.iter().any(|tx| &tx.hash == hash)
    }

    /// Every pending transaction, oldest first: the candidates for the next
    /// block.
    ///
    /// This used to stop at `BLOCK_GAS_LIMIT / TRANSFER_GAS` = 149, which was
    /// right only while every transaction cost exactly 21,000 gas. How many
    /// actually fit now depends on how much gas each one *uses*, which is only
    /// known by executing it — so the executor decides, filling the block until
    /// the next transaction would not fit.
    ///
    /// This does not remove them. The miner may spend a long time failing to seal
    /// a block, and a transaction must stay pending until it is actually mined.
    pub fn peek_mempool_batch(&self) -> Vec<PendingTransaction> {
        self.mempool.clone()
    }

    // block production

    /// Installs a sealed block as the new chain head.
    ///
    /// `world` is the state the miner produced by executing `transactions`, and
    /// it replaces the head state wholesale. This is safe because the miner is
    /// the only writer of account state; the JSON-RPC server only ever appends to
    /// the mempool.
    pub fn commit_block(
        &mut self,
        world: WorldState,
        block: Block,
        transactions: Vec<Transaction>,
        receipts: Vec<TransactionReceipt>,
    ) {
        let mined: Vec<B256> = transactions.iter().map(|tx| tx.hash).collect();
        self.mempool
            .retain(|pending| !mined.contains(&pending.hash));

        self.world = world;
        self.block_index.insert(block.hash, block.number);
        for tx in transactions {
            self.transactions.insert(tx.hash, tx);
        }
        for receipt in receipts {
            self.receipts.insert(receipt.transaction_hash, receipt);
        }
        self.blocks.push(block);
        self.state_history.push(self.world.clone());
    }

    /// Default gas price for a node with no fee market: a constant.
    pub fn gas_price(&self) -> u128 {
        GAS_PRICE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_zero_bits_counts_what_it_says() {
        assert_eq!(leading_zero_bits(&B256::ZERO), 256);
        assert_eq!(leading_zero_bits(&B256::repeat_byte(0xff)), 0);

        // 0x00 0x0f ... = 8 zero bits, then 0b00001111 contributes 4 more.
        let mut bytes = [0u8; 32];
        bytes[1] = 0x0f;
        assert_eq!(leading_zero_bits(&B256::new(bytes)), 12);
    }

    #[test]
    fn difficulty_is_only_retargeted_when_a_whole_bit_is_warranted() {
        // Far too fast (more than 2x) - demand another bit.
        assert_eq!(retarget_difficulty(20, 100, 2_000), 21);
        // Far too slow (more than 2x) - give a bit back.
        assert_eq!(retarget_difficulty(20, 5_000, 2_000), 19);
        // Inside the band a single bit can resolve - leave it alone, so the
        // difficulty settles instead of oscillating.
        assert_eq!(retarget_difficulty(20, 1_500, 2_000), 20);
        assert_eq!(retarget_difficulty(20, 2_000, 2_000), 20);
        assert_eq!(retarget_difficulty(20, 3_900, 2_000), 20);
    }

    #[test]
    fn difficulty_stays_inside_its_clamps() {
        assert_eq!(
            retarget_difficulty(MIN_DIFFICULTY_LEADING_ZEROS, u64::MAX, 1),
            MIN_DIFFICULTY_LEADING_ZEROS
        );
        assert_eq!(
            retarget_difficulty(MAX_DIFFICULTY_LEADING_ZEROS, 0, u64::MAX),
            MAX_DIFFICULTY_LEADING_ZEROS
        );
    }

    #[test]
    fn a_fresh_chain_has_a_genesis_block_and_funded_accounts() {
        let address = Address::repeat_byte(0x11);
        let genesis = BreejaGenesis {
            accounts: vec![(address, U256::from(1_000u64))],
        };
        let state = BlockState::new_with_gensis(genesis, 8);

        assert_eq!(state.get_block_number(), 0);
        assert_eq!(state.head().number, 0);
        assert_eq!(state.head().parent_hash, B256::ZERO);
        assert_eq!(state.get_account(&address).balance, U256::from(1_000u64));
        assert_eq!(state.get_account(&address).nonce, 0);
        // An address nobody funded reads as empty, not missing.
        assert_eq!(state.get_account(&Address::ZERO).balance, U256::ZERO);
    }

    #[test]
    fn the_genesis_block_is_reachable_by_hash_and_by_number() {
        let state = BlockState::new_with_gensis(BreejaGenesis::default(), 8);
        let hash = state.head().hash;
        assert_eq!(state.get_block_by_hash(&hash).unwrap().number, 0);
        assert_eq!(state.get_block(0).unwrap().hash, hash);
        assert!(state.get_block(1).is_none());
    }

    #[test]
    fn the_state_root_commits_to_balances_and_ignores_map_ordering() {
        let a = Address::repeat_byte(0x01);
        let b = Address::repeat_byte(0x02);

        let mut one = HashMap::new();
        one.insert(a, Account::new(U256::from(1u64)));
        one.insert(b, Account::new(U256::from(2u64)));

        // Same accounts inserted in the opposite order must hash identically.
        let mut two = HashMap::new();
        two.insert(b, Account::new(U256::from(2u64)));
        two.insert(a, Account::new(U256::from(1u64)));

        // `state_root_of` takes the whole world now, so that code and storage are
        // committed to as well; these accounts simply have neither.
        let root = |accounts: &HashMap<Address, Account>| {
            state_root_of(&WorldState::from_accounts(accounts.clone()))
        };
        assert_eq!(root(&one), root(&two));

        // Changing a single balance must change the root.
        let mut three = one.clone();
        three.insert(a, Account::new(U256::from(99u64)));
        assert_ne!(root(&one), root(&three));
    }

    #[test]
    fn states_differing_in_one_storage_slot_have_different_roots() {
        let contract = Address::repeat_byte(0xc0);
        let mut one = WorldState::default();
        one.set_code(contract, Bytes::from_static(&[0x00]));
        one.set_storage(contract, U256::ZERO, U256::from(42u64));
        one.refresh_storage_root(contract);

        let mut two = one.clone();
        two.set_storage(contract, U256::ZERO, U256::from(43u64));
        two.refresh_storage_root(contract);
        assert_ne!(state_root_of(&one), state_root_of(&two));

        // Even a write whose cached `storage_root` was never refreshed changes
        // the root, because the root is computed from the slots themselves.
        let mut stale = one.clone();
        stale.set_storage(contract, U256::from(1u64), U256::from(1u64));
        assert_eq!(
            stale.account(&contract).storage_root,
            one.account(&contract).storage_root
        );
        assert_ne!(state_root_of(&one), state_root_of(&stale));
    }

    #[test]
    fn states_differing_only_in_code_have_different_roots() {
        let contract = Address::repeat_byte(0xc0);
        let mut one = WorldState::default();
        one.set_code(contract, Bytes::from_static(&[0x00]));
        let mut two = WorldState::default();
        two.set_code(contract, Bytes::from_static(&[0xfe]));
        assert_ne!(state_root_of(&one), state_root_of(&two));
    }

    #[test]
    fn the_logs_bloom_matches_alloys_and_finds_what_was_logged() {
        let emitter = Address::repeat_byte(0xc0);
        let topic = B256::repeat_byte(0x7e);
        let logs = [
            Log {
                address: emitter,
                topics: vec![topic],
                ..Default::default()
            },
            Log {
                address: Address::repeat_byte(0xc1),
                ..Default::default()
            },
        ];

        let bloom = logs_bloom(&logs);
        assert_ne!(bloom, Bloom::ZERO);

        // The same algorithm as alloy's reference implementation, bit for bit.
        let mut reference = Bloom::ZERO;
        for log in &logs {
            reference.accrue_raw_log(log.address, &log.topics);
        }
        assert_eq!(bloom, reference);

        // Everything logged is reported as possibly present.
        assert!(bloom.contains_input(alloy::primitives::BloomInput::Raw(emitter.as_slice())));
        assert!(bloom.contains_input(alloy::primitives::BloomInput::Raw(topic.as_slice())));

        // No logs, no bits.
        assert_eq!(logs_bloom(&[]), Bloom::ZERO);
    }

    #[test]
    fn pending_nonce_and_balance_account_for_what_is_already_queued() {
        let sender = Address::repeat_byte(0x11);
        let one_eth = U256::from(10u64).pow(U256::from(18));
        let genesis = BreejaGenesis {
            accounts: vec![(sender, one_eth * U256::from(10u64))],
        };
        let mut state = BlockState::new_with_gensis(genesis, 8);

        assert_eq!(state.get_pending_nonce(&sender), 0);

        state.add_to_mempool(PendingTransaction {
            hash: B256::repeat_byte(0xaa),
            from: sender,
            // `to` became optional and `input` was added when contracts did.
            to: Some(Address::repeat_byte(0x22)),
            value: one_eth,
            nonce: 0,
            gas: TRANSFER_GAS,
            gas_price: GAS_PRICE,
            input: Bytes::new(),
            chain_id: 1,
            v: 0,
            r: U256::ZERO,
            s: U256::ZERO,
        });

        // The next transaction must use nonce 1, and may only spend what the
        // queued one leaves behind.
        assert_eq!(state.get_pending_nonce(&sender), 1);
        assert!(state.get_pending_balance(&sender) < one_eth * U256::from(9u64));
        assert_eq!(state.mempool_len(), 1);
        assert!(state.mempool_contains(&B256::repeat_byte(0xaa)));
    }

    #[test]
    fn code_is_stored_once_by_hash_and_found_by_address() {
        let a = Address::repeat_byte(0x01);
        let b = Address::repeat_byte(0x02);
        let code = Bytes::from_static(&[0x60, 0x2a, 0x00]);

        let mut world = WorldState::default();
        world.set_code(a, code.clone());
        world.set_code(b, code.clone());

        assert_eq!(world.code_of(&a), code);
        assert_eq!(world.code_of(&b), code);
        assert_eq!(world.account(&a).code_hash, keccak256(&code));
        // Two contracts, one copy of the bytecode.
        assert_eq!(world.code.len(), 1);
        // An account with no code reads as empty code.
        assert!(world.code_of(&Address::repeat_byte(0x03)).is_empty());
    }

    #[test]
    fn writing_zero_deletes_a_slot_so_equal_storage_hashes_equal() {
        let a = Address::repeat_byte(0x01);
        let mut world = WorldState::default();
        world.account_mut(a);
        let empty_root = {
            world.refresh_storage_root(a);
            world.account(&a).storage_root
        };
        assert_eq!(empty_root, B256::ZERO);

        world.set_storage(a, U256::from(7), U256::from(42));
        world.refresh_storage_root(a);
        assert_eq!(world.storage_at(&a, U256::from(7)), U256::from(42));
        assert_ne!(world.account(&a).storage_root, B256::ZERO);

        // Clearing the slot returns the account to exactly the state it had
        // before it was ever written.
        world.set_storage(a, U256::from(7), U256::ZERO);
        world.refresh_storage_root(a);
        assert_eq!(world.storage_at(&a, U256::from(7)), U256::ZERO);
        assert!(!world.storage.contains_key(&a));
        assert_eq!(world.account(&a).storage_root, B256::ZERO);
    }

    #[test]
    fn the_storage_root_changes_with_any_slot_and_ignores_map_ordering() {
        let mut one = HashMap::new();
        one.insert(U256::from(1), U256::from(10));
        one.insert(U256::from(2), U256::from(20));

        let mut two = HashMap::new();
        two.insert(U256::from(2), U256::from(20));
        two.insert(U256::from(1), U256::from(10));
        assert_eq!(storage_root_of(&one), storage_root_of(&two));

        two.insert(U256::from(2), U256::from(21));
        assert_ne!(storage_root_of(&one), storage_root_of(&two));
    }

    #[test]
    fn removing_an_account_drops_its_storage() {
        let a = Address::repeat_byte(0x01);
        let mut world = WorldState::default();
        world.account_mut(a).balance = U256::from(5);
        world.set_storage(a, U256::from(1), U256::from(1));

        world.remove_account(&a);
        assert_eq!(world.account(&a), Account::default());
        assert_eq!(world.storage_at(&a, U256::from(1)), U256::ZERO);
    }

    #[test]
    fn intrinsic_gas_charges_for_calldata_and_creation() {
        // A plain transfer: exactly the old constant.
        assert_eq!(intrinsic_gas(&[], false), 21_000);
        // Zero bytes are cheap, non-zero bytes are not.
        assert_eq!(intrinsic_gas(&[0, 0], false), 21_000 + 8);
        assert_eq!(intrinsic_gas(&[1, 2], false), 21_000 + 32);
        // A deployment: +32,000, plus 2 gas per started 32-byte word of initcode.
        assert_eq!(intrinsic_gas(&[1; 33], true), 21_000 + 33 * 16 + 32_000 + 2 * 2);
    }

    #[test]
    fn recent_block_hashes_include_genesis_on_a_fresh_chain() {
        let state = BlockState::new_with_gensis(BreejaGenesis::default(), 8);
        let hashes = state.recent_block_hashes(0);
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[&0], state.head().hash);
        // Asking past the head never invents blocks.
        assert_eq!(state.recent_block_hashes(10), hashes);
    }
}
