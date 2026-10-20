//! The bridge between Breeja's state and REVM.
//!
//! REVM does not take state as an argument. It *asks* for it, through the
//! [`Database`] trait, whenever execution needs an account, a piece of code or a
//! storage slot — because until a contract runs, nobody knows what it will touch.
//! When the transaction finishes, REVM hands back every account it changed, and
//! [`DatabaseCommit`] writes those changes into the state.
//!
//! [`BreejaDb`] owns a *snapshot* of the world, never the live chain state. REVM
//! borrows its database mutably for the whole of a transaction, and the live
//! state sits behind an `RwLock` the JSON-RPC server also reads. Executing against
//! a copy is what lets a long contract call run without freezing every RPC
//! request — the same snapshot-execute-commit shape the miner already uses.

use std::{collections::HashMap, convert::Infallible};

use alloy::primitives::{Address, B256, Bytes, U256};
use primitives::{
    constants::{BLOCK_GAS_LIMIT, CHAIN_ID},
    node::{PendingTransaction, WorldState},
};
use revm::{
    Context, Database, DatabaseCommit, DatabaseRef, ExecuteEvm, MainBuilder, MainContext,
    bytecode::Bytecode,
    context::{
        BlockEnv, CfgEnv, TxEnv,
        result::{EVMError, ExecutionResult},
    },
    database_interface::WrapDatabaseRef,
    primitives::{AddressMap, KECCAK_EMPTY, StorageKey, StorageValue, TxKind, hardfork::SpecId},
    state::{Account as EvmAccount, AccountInfo},
};

/// Why REVM refused to execute a transaction at all.
///
/// This is *not* a revert. An `Err` here means the transaction was invalid —
/// wrong nonce, unable to afford its gas, a gas limit above the block's — and
/// it must be skipped, not mined. A transaction that runs and then fails is an
/// `Ok` with an unsuccessful outcome.
pub type InvalidTransaction = EVMError<Infallible>;

/// What the EVM needs to know about the block a transaction runs in.
///
/// All of it must be known *before* execution, which is before mining: the
/// block's own hash does not exist yet, because finding it is what proof of
/// work does next.
#[derive(Debug, Clone)]
pub struct BlockContext {
    pub number: u64,
    pub timestamp: u64,
    /// Receives the transaction fees. REVM pays this address itself.
    pub miner: Address,
    pub difficulty: u8,
    pub parent_hash: B256,
}

/// What executing one transaction produced.
#[derive(Debug, Clone)]
pub struct TxOutcome {
    /// `true` if execution completed; `false` if it reverted or halted. Either
    /// way the transaction was valid, is mined, and paid for its gas.
    pub success: bool,
    /// Gas actually charged, after refunds.
    pub gas_used: u64,
    /// The new contract's address, for a deployment that succeeded.
    pub created_address: Option<Address>,
    /// Events the code emitted. Always empty for a failed transaction: a revert
    /// undoes its logs along with everything else.
    pub logs: Vec<revm::primitives::Log>,
    /// What the code returned: a call's return data, a deployment's runtime
    /// code, or a revert's reason.
    pub output: Bytes,
    /// Why execution halted — out of gas, an invalid opcode — if it did. `None`
    /// for both success and an explicit `REVERT`.
    pub halt_reason: Option<String>,
}

impl TxOutcome {
    /// `true` for an explicit `REVERT`, as opposed to a halt. Only a revert
    /// carries a reason the caller chose to return.
    pub fn reverted(&self) -> bool {
        !self.success && self.halt_reason.is_none()
    }
}

fn summarize(result: &ExecutionResult) -> TxOutcome {
    TxOutcome {
        success: result.is_success(),
        gas_used: result.tx_gas_used(),
        created_address: result.created_address(),
        logs: result.logs().to_vec(),
        output: result.output().cloned().unwrap_or_default(),
        halt_reason: match result {
            ExecutionResult::Halt { reason, .. } => Some(format!("{reason:?}")),
            _ => None,
        },
    }
}

impl BlockContext {
    fn block_env(&self) -> BlockEnv {
        BlockEnv {
            number: U256::from(self.number),
            // REVM credits `gas_used * gas_price` to this address. Breeja must
            // not pay the fee a second time by hand.
            beneficiary: self.miner,
            timestamp: U256::from(self.timestamp),
            gas_limit: BLOCK_GAS_LIMIT,
            // Breeja is pre-EIP-1559: one fixed gas price and no burn. Any
            // non-zero basefee would destroy part of every fee.
            basefee: 0,
            difficulty: U256::from(self.difficulty),
            // Under Shanghai rules `DIFFICULTY` reads this instead. The parent's
            // hash is the freshest randomness that exists before mining.
            prevrandao: Some(self.parent_hash),
            ..Default::default()
        }
    }
}

/// Frontier-era consensus, Shanghai-era execution: modern Solidity needs
/// opcodes like `PUSH0` that Frontier never had, and Shanghai is the newest
/// fork that does not also need blob-gas fields in the block.
fn cfg_env() -> CfgEnv {
    CfgEnv::new_with_spec(SpecId::SHANGHAI).with_chain_id(CHAIN_ID)
}

fn tx_env(tx: &PendingTransaction) -> TxEnv {
    TxEnv {
        tx_type: 0, // legacy
        caller: tx.from,
        gas_limit: tx.gas,
        gas_price: tx.gas_price,
        kind: tx.to.map_or(TxKind::Create, TxKind::Call),
        value: tx.value,
        data: tx.input.clone(),
        nonce: tx.nonce,
        chain_id: Some(tx.chain_id),
        ..Default::default()
    }
}

/// Runs one transaction through REVM and commits its effects to `db`.
///
/// REVM does the whole state transition: it checks the nonce and balance,
/// charges the sender for the full gas limit, moves the value, refunds unused
/// gas, bumps the nonce and pays the fee to the block's beneficiary. None of
/// that is repeated here.
pub fn execute_transaction(
    db: &mut BreejaDb,
    block: &BlockContext,
    tx: &PendingTransaction,
) -> Result<TxOutcome, InvalidTransaction> {
    let mut evm = Context::mainnet()
        .with_db(&mut *db)
        .with_block(block.block_env())
        .with_cfg(cfg_env())
        .build_mainnet();

    let outcome = evm.transact(tx_env(tx))?;
    let summary = summarize(&outcome.result);

    // The EVM holds `db` mutably for as long as it lives, so it has to go
    // before the changes can be written back.
    drop(evm);
    db.commit(outcome.state);

    Ok(summary)
}

/// A transaction-shaped question for the EVM that is never mined: what
/// `eth_call` and `eth_estimateGas` ask.
#[derive(Debug, Clone, Default)]
pub struct SimulatedCall {
    pub from: Address,
    /// `None` simulates a deployment.
    pub to: Option<Address>,
    pub value: U256,
    pub data: Bytes,
    /// Zero unless the caller names a price. With a zero price the gas costs
    /// nothing, so the balance check only has to cover `value`.
    pub gas_price: u128,
    /// Whether `from` must be able to afford the call. `eth_call` turns this off
    /// — reading a contract should not need funds — while `eth_estimateGas`
    /// keeps it on, because an estimate for a transaction the sender cannot pay
    /// for describes a transaction that can never be sent.
    pub check_balance: bool,
}

/// Executes `call` against `db` and throws every state change away.
///
/// That discard is the *only* difference from a mined transaction. REVM keeps
/// its writes in a journal; reading through `WrapDatabaseRef` means nothing ever
/// reaches `db`, so the same snapshot can be simulated against again and again.
///
/// The nonce check is always off: a call request carries no nonce, and asking
/// the caller to supply the right one would add nothing to the answer.
pub fn simulate(
    db: &BreejaDb,
    block: &BlockContext,
    call: &SimulatedCall,
    gas_limit: u64,
) -> Result<TxOutcome, InvalidTransaction> {
    let mut cfg = cfg_env();
    cfg.disable_nonce_check = true;
    cfg.disable_balance_check = !call.check_balance;

    let tx = TxEnv {
        tx_type: 0,
        caller: call.from,
        gas_limit,
        gas_price: call.gas_price,
        kind: call.to.map_or(TxKind::Create, TxKind::Call),
        value: call.value,
        data: call.data.clone(),
        chain_id: Some(CHAIN_ID),
        ..Default::default()
    };

    let mut evm = Context::mainnet()
        .with_db(WrapDatabaseRef(db))
        .with_block(block.block_env())
        .with_cfg(cfg)
        .build_mainnet();

    // `state` is dropped without being committed.
    Ok(summarize(&evm.transact(tx)?.result))
}

/// Why a gas estimate could not be produced.
#[derive(Debug)]
pub enum EstimateError {
    /// REVM refused the call outright, for example because `from` cannot
    /// afford `value`.
    Invalid(InvalidTransaction),
    /// The call fails even with all the gas it was allowed. More gas would not
    /// help, so there is no amount to estimate.
    Failed(TxOutcome),
}

/// Finds the smallest gas limit at which `call` succeeds, searching up to `cap`.
///
/// The cost of running code is only known by running it, and running it with
/// too little gas fails — so this is a search. One run at the cap proves that
/// *some* limit works and reports the gas it used. No smaller limit can work,
/// so that is the floor; most calls succeed right at it and the search ends
/// there. When they do not — a refund made the reported figure smaller than
/// what the call needed while it ran, or the 63/64 rule held gas back from a
/// sub-call — a binary search finds the true minimum. Every candidate it returns
/// has actually succeeded.
///
/// This is a heuristic, not a proof: a contract can read its remaining gas with
/// the `GAS` opcode and behave differently on each probe.
pub fn estimate_gas(
    db: &BreejaDb,
    block: &BlockContext,
    call: &SimulatedCall,
    cap: u64,
) -> Result<u64, EstimateError> {
    let succeeds = |gas: u64| simulate(db, block, call, gas).is_ok_and(|outcome| outcome.success);

    let at_cap = simulate(db, block, call, cap).map_err(EstimateError::Invalid)?;
    if !at_cap.success {
        return Err(EstimateError::Failed(at_cap));
    }

    if succeeds(at_cap.gas_used) {
        return Ok(at_cap.gas_used);
    }

    // `lo` always fails and `hi` always succeeds.
    let (mut lo, mut hi) = (at_cap.gas_used, cap);
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        if succeeds(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Ok(hi)
}

/// A snapshot of chain state that REVM can read from and write back to.
#[derive(Debug, Clone, Default)]
pub struct BreejaDb {
    pub world: WorldState,
    /// Recent block hashes by number, backing the `BLOCKHASH` opcode.
    pub block_hashes: HashMap<u64, B256>,
}

impl BreejaDb {
    pub fn new(world: WorldState, block_hashes: HashMap<u64, B256>) -> Self {
        Self {
            world,
            block_hashes,
        }
    }

    /// Hands the (possibly modified) world back, ending the EVM's use of it.
    pub fn into_world(self) -> WorldState {
        self.world
    }
}

/// Breeja records "no code" as a zero hash; the EVM expects `keccak256("")`.
/// Every code hash crossing into REVM goes through here.
fn evm_code_hash(code_hash: B256) -> B256 {
    if code_hash == B256::ZERO {
        KECCAK_EMPTY
    } else {
        code_hash
    }
}

/// Read-only access, which is all a simulation needs.
///
/// `eth_call` and `eth_estimateGas` run REVM against `&BreejaDb` through
/// `WrapDatabaseRef`: REVM keeps its writes in its own journal, nothing is ever
/// committed, and one snapshot can serve every probe of a gas estimate without
/// being copied again.
impl DatabaseRef for BreejaDb {
    // Reading from an in-memory map cannot fail. A database backed by disk would
    // use a real error type here.
    type Error = Infallible;

    /// An address Breeja has never seen is reported as *non-existent* (`None`),
    /// not as an empty account. The distinction is real in the EVM: for example,
    /// a `CALL` that sends value to a non-existent account pays an extra 25,000
    /// gas to create it.
    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let Some(account) = self.world.accounts.get(&address) else {
            return Ok(None);
        };

        let code = self.world.code_of(&address);
        let bytecode = if code.is_empty() {
            Bytecode::default()
        } else {
            Bytecode::new_raw(code)
        };

        Ok(Some(AccountInfo {
            balance: account.balance,
            nonce: account.nonce,
            code_hash: evm_code_hash(account.code_hash),
            // Handing the code over now means REVM never has to come back and
            // ask for it via `code_by_hash`.
            code: Some(bytecode),
            ..Default::default()
        }))
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        // Unknown hashes read as empty code. A disk-backed node would treat a
        // miss as corruption, but `Infallible` leaves no way to say so — and
        // `basic` always supplies the code up front, so this is a fallback.
        Ok(self
            .world
            .code
            .get(&code_hash)
            .cloned()
            .map(Bytecode::new_raw)
            .unwrap_or_default())
    }

    fn storage_ref(&self, address: Address, index: StorageKey) -> Result<StorageValue, Self::Error> {
        Ok(self.world.storage_at(&address, index))
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        Ok(self.block_hashes.get(&number).copied().unwrap_or_default())
    }
}

/// Mutable access, which mined transactions need so their changes can be
/// committed. Reading is the same either way.
impl Database for BreejaDb {
    type Error = Infallible;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.basic_ref(address)
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.code_by_hash_ref(code_hash)
    }

    fn storage(&mut self, address: Address, index: StorageKey) -> Result<StorageValue, Self::Error> {
        self.storage_ref(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.block_hash_ref(number)
    }
}

impl DatabaseCommit for BreejaDb {
    fn commit(&mut self, changes: AddressMap<EvmAccount>) {
        for (address, account) in changes {
            // REVM reports every account it *looked at*, not only the ones it
            // changed. Untouched accounts are skipped, so a read never writes.
            if !account.is_touched() {
                continue;
            }

            // `SELFDESTRUCT` deletes an account outright — something Breeja had
            // no concept of before contracts.
            if account.is_selfdestructed() {
                self.world.remove_account(&address);
                continue;
            }

            // A freshly created contract starts with empty storage, even if the
            // address once held a contract that self-destructed. REVM's own
            // `CacheDB` clears here for the same reason.
            if account.is_created() {
                self.world.storage.remove(&address);
            }

            let entry = self.world.account_mut(address);
            entry.balance = account.info.balance;
            entry.nonce = account.info.nonce;

            if let Some(code) = &account.info.code
                && !code.is_empty()
                && evm_code_hash(entry.code_hash) != account.info.code_hash
            {
                // `original_bytes`, not `bytes`: REVM pads analysed legacy code
                // internally, and that padding must not leak into `eth_getCode`.
                self.world.set_code(address, code.original_bytes());
            }

            for (slot, value) in account.storage {
                self.world.set_storage(address, slot, value.present_value);
            }
            self.world.refresh_storage_root(address);
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{Bytes, U256, keccak256};
    use primitives::node::Account;
    use revm::state::EvmStorageSlot;

    use super::*;

    fn one_eth() -> U256 {
        U256::from(10u64).pow(U256::from(18))
    }

    fn db_with(address: Address, account: Account) -> BreejaDb {
        let mut world = WorldState::default();
        world.accounts.insert(address, account);
        BreejaDb::new(world, HashMap::new())
    }

    /// The runtime of the brief's test contract: `sstore(0, 42)`, `LOG0`, `STOP`.
    const STORE42_RUNTIME: [u8; 11] = [
        0x60, 0x2a, 0x60, 0x00, 0x55, 0x60, 0x00, 0x60, 0x00, 0xa0, 0x00,
    ];

    const RETURN42_RUNTIME: [u8; 10] = [0x60, 0x2a, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3];
    const REVERTER_RUNTIME: [u8; 5] = [0x60, 0x00, 0x60, 0x00, 0xfd];

    fn sim_block() -> BlockContext {
        BlockContext {
            number: 1,
            timestamp: 1,
            miner: Address::repeat_byte(0x99),
            difficulty: 1,
            parent_hash: B256::repeat_byte(0x01),
        }
    }

    /// A funded caller plus three contracts, installed directly rather than
    /// deployed, so each test starts from known code.
    fn world_with_contracts() -> (BreejaDb, Address) {
        let caller = Address::repeat_byte(0x11);
        let mut db = db_with(caller, Account::new(one_eth()));
        for (byte, code) in [
            (0xa1, &STORE42_RUNTIME[..]),
            (0xa2, &RETURN42_RUNTIME[..]),
            (0xa3, &REVERTER_RUNTIME[..]),
        ] {
            db.world
                .set_code(Address::repeat_byte(byte), Bytes::copy_from_slice(code));
        }
        (db, caller)
    }

    fn call_to(from: Address, to: Address) -> SimulatedCall {
        SimulatedCall {
            from,
            to: Some(to),
            check_balance: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_simulation_returns_the_output_and_changes_nothing() {
        let (db, caller) = world_with_contracts();
        let before = db.world.clone();

        let answer = simulate(
            &db,
            &sim_block(),
            &call_to(caller, Address::repeat_byte(0xa2)),
            BLOCK_GAS_LIMIT,
        )
        .unwrap();
        assert!(answer.success);
        assert_eq!(answer.output, Bytes::from(U256::from(42u64).to_be_bytes_vec()));

        // STORE42 *would* write slot 0 and emit a log...
        let store = simulate(
            &db,
            &sim_block(),
            &call_to(caller, Address::repeat_byte(0xa1)),
            BLOCK_GAS_LIMIT,
        )
        .unwrap();
        assert!(store.success);
        assert_eq!(store.logs.len(), 1);
        // ...but a simulation keeps none of it: not the slot, not the caller's
        // nonce, not the fee.
        assert_eq!(db.world, before);
    }

    #[test]
    fn eth_call_needs_no_funds_but_an_estimate_does() {
        let (db, _) = world_with_contracts();
        let pauper = Address::repeat_byte(0x77);
        let mut call = call_to(pauper, Address::repeat_byte(0x22));
        call.value = U256::from(1u64);

        call.check_balance = false;
        assert!(simulate(&db, &sim_block(), &call, BLOCK_GAS_LIMIT).unwrap().success);

        call.check_balance = true;
        assert!(simulate(&db, &sim_block(), &call, BLOCK_GAS_LIMIT).is_err());
    }

    #[test]
    fn a_transfer_is_estimated_at_exactly_21000() {
        let (db, caller) = world_with_contracts();
        let mut call = call_to(caller, Address::repeat_byte(0x22));
        call.value = one_eth() / U256::from(2u64);

        let gas = estimate_gas(&db, &sim_block(), &call, BLOCK_GAS_LIMIT).unwrap();
        assert_eq!(gas, 21_000);
    }

    #[test]
    fn an_estimate_is_enough_and_one_less_is_not() {
        let (db, caller) = world_with_contracts();
        let call = call_to(caller, Address::repeat_byte(0xa1));

        let gas = estimate_gas(&db, &sim_block(), &call, BLOCK_GAS_LIMIT).unwrap();
        assert!(simulate(&db, &sim_block(), &call, gas).unwrap().success);
        let short = simulate(&db, &sim_block(), &call, gas - 1).unwrap();
        assert!(!short.success, "the estimate should be the minimum");
        assert!(short.halt_reason.is_some(), "too little gas halts, it does not revert");
    }

    #[test]
    fn a_call_that_always_reverts_cannot_be_estimated() {
        let (db, caller) = world_with_contracts();
        let call = call_to(caller, Address::repeat_byte(0xa3));

        match estimate_gas(&db, &sim_block(), &call, BLOCK_GAS_LIMIT) {
            Err(EstimateError::Failed(outcome)) => assert!(outcome.reverted()),
            other => panic!("expected a revert at the cap, got {other:?}"),
        }
    }

    #[test]
    fn basic_returns_balance_and_nonce_for_a_funded_account() {
        let alice = Address::repeat_byte(0x11);
        let mut db = db_with(
            alice,
            Account {
                nonce: 3,
                balance: one_eth(),
                ..Default::default()
            },
        );

        let info = db.basic(alice).unwrap().expect("a funded account exists");
        assert_eq!(info.balance, one_eth());
        assert_eq!(info.nonce, 3);
        // No code: REVM must see the empty-code hash, not Breeja's zero.
        assert_eq!(info.code_hash, KECCAK_EMPTY);
        assert!(info.code.unwrap().is_empty());
    }

    #[test]
    fn basic_returns_none_for_an_unknown_address() {
        // Deliberately `None`, not an empty account: REVM then treats the
        // address as non-existent, which is what decides whether sending it
        // value pays the 25,000 gas account-creation surcharge.
        let mut db = BreejaDb::default();
        assert!(db.basic(Address::repeat_byte(0x42)).unwrap().is_none());
    }

    #[test]
    fn basic_and_code_by_hash_serve_a_contracts_code() {
        let contract = Address::repeat_byte(0xc0);
        let code = Bytes::from_static(&STORE42_RUNTIME);
        let mut db = BreejaDb::default();
        db.world.account_mut(contract);
        db.world.set_code(contract, code.clone());

        let info = db.basic(contract).unwrap().unwrap();
        assert_eq!(info.code_hash, keccak256(&code));
        assert_eq!(info.code.unwrap().original_bytes(), code);
        assert_eq!(
            db.code_by_hash(keccak256(&code)).unwrap().original_bytes(),
            code
        );
    }

    #[test]
    fn storage_and_block_hash_read_through_to_the_snapshot() {
        let contract = Address::repeat_byte(0xc0);
        let mut db = BreejaDb::default();
        db.world.set_storage(contract, U256::from(7), U256::from(42));
        db.block_hashes.insert(5, B256::repeat_byte(0x55));

        assert_eq!(db.storage(contract, U256::from(7)).unwrap(), U256::from(42));
        assert_eq!(db.storage(contract, U256::from(8)).unwrap(), U256::ZERO);
        assert_eq!(db.block_hash(5).unwrap(), B256::repeat_byte(0x55));
        assert_eq!(db.block_hash(6).unwrap(), B256::ZERO);
    }

    #[test]
    fn commit_persists_balance_nonce_code_and_storage() {
        let contract = Address::repeat_byte(0xc0);
        let code = Bytes::from_static(&STORE42_RUNTIME);
        let mut db = BreejaDb::default();

        // Hand-build what REVM would report after deploying a contract that
        // writes slot 0 = 42 in its constructor.
        let bytecode = Bytecode::new_raw(code.clone());
        let info = AccountInfo {
            balance: U256::from(1_000u64),
            nonce: 1,
            code_hash: bytecode.hash_slow(),
            code: Some(bytecode),
            ..Default::default()
        };
        let changed = EvmAccount::from(info)
            .with_storage(std::iter::once((
                U256::ZERO,
                EvmStorageSlot::new_changed(U256::ZERO, U256::from(42), Default::default()),
            )))
            .with_created_mark()
            .with_touched_mark();

        let mut changes = AddressMap::default();
        changes.insert(contract, changed);
        db.commit(changes);

        let account = db.world.account(&contract);
        assert_eq!(account.balance, U256::from(1_000u64));
        assert_eq!(account.nonce, 1);
        assert_eq!(account.code_hash, keccak256(&code));
        assert_eq!(db.world.code_of(&contract), code);
        assert_eq!(db.world.storage_at(&contract, U256::ZERO), U256::from(42));
        assert_ne!(account.storage_root, B256::ZERO);

        // And the committed state reads back through the EVM's own interface.
        let info = db.basic(contract).unwrap().unwrap();
        assert_eq!(info.code.unwrap().original_bytes(), code);
        assert_eq!(db.storage(contract, U256::ZERO).unwrap(), U256::from(42));
    }

    #[test]
    fn untouched_accounts_are_not_committed() {
        let alice = Address::repeat_byte(0x11);
        let mut db = db_with(alice, Account::new(one_eth()));

        // REVM *read* this account and reports it, but never touched it.
        let looked_at = EvmAccount::from(AccountInfo {
            balance: U256::ZERO,
            ..Default::default()
        });
        let mut changes = AddressMap::default();
        changes.insert(alice, looked_at);
        db.commit(changes);

        assert_eq!(db.world.account(&alice).balance, one_eth());
    }

    #[test]
    fn a_self_destructed_account_is_removed_with_its_storage() {
        let contract = Address::repeat_byte(0xc0);
        let mut db = BreejaDb::default();
        db.world.account_mut(contract).balance = U256::from(5u64);
        db.world.set_storage(contract, U256::ZERO, U256::from(1u64));

        let destroyed = EvmAccount::from(AccountInfo::default())
            .with_selfdestruct_mark()
            .with_touched_mark();
        let mut changes = AddressMap::default();
        changes.insert(contract, destroyed);
        db.commit(changes);

        assert!(!db.world.accounts.contains_key(&contract));
        assert_eq!(db.world.storage_at(&contract, U256::ZERO), U256::ZERO);
    }

    #[test]
    fn a_storage_write_back_to_zero_clears_the_slot() {
        let contract = Address::repeat_byte(0xc0);
        let mut db = BreejaDb::default();
        db.world.account_mut(contract);
        db.world.set_storage(contract, U256::ZERO, U256::from(42));
        db.world.refresh_storage_root(contract);

        let cleared = EvmAccount::from(AccountInfo::default())
            .with_storage(std::iter::once((
                U256::ZERO,
                EvmStorageSlot::new_changed(U256::from(42), U256::ZERO, Default::default()),
            )))
            .with_touched_mark();
        let mut changes = AddressMap::default();
        changes.insert(contract, cleared);
        db.commit(changes);

        assert_eq!(db.world.storage_at(&contract, U256::ZERO), U256::ZERO);
        assert_eq!(db.world.account(&contract).storage_root, B256::ZERO);
    }
}
