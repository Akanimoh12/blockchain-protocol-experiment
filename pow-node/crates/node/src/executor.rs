use std::collections::HashMap;

use alloy::primitives::{Address, B256};
use primitives::{
    constants::BLOCK_REWARD,
    node::{
        Account, PendingTransaction, Transaction, TransactionReceipt, WorldState, hash_list_root,
    },
};

use crate::evm::{BlockContext, BreejaDb, execute_transaction};

/// What executing a batch of transactions produced.
pub struct Execution {
    /// World state after the batch, ready to become the new head state.
    pub world: WorldState,
    pub transactions: Vec<Transaction>,
    pub receipts: Vec<TransactionReceipt>,
    pub gas_used: u64,
    pub transaction_root: B256,
    pub receipts_root: B256,
    pub state_root: B256,
}

/// Pays the miner for sealing a block.
///
/// This is the only way new ETH enters the supply, and it is what makes mining
/// worth the electricity. Breeja pays Frontier's 5 ETH; Ethereum reduced this
/// twice before dropping it entirely at the merge.
///
/// REVM pays the miner its *fees*, but it knows nothing about block rewards —
/// those are this node's policy, so this node pays them.
pub fn pay_block_reward(
    accounts: &mut HashMap<Address, Account>,
    miner: Address,
) -> Result<(), anyhow::Error> {
    accounts.entry(miner).or_default().credit(BLOCK_REWARD)?;
    Ok(())
}

/// Executes a batch of pending transactions through the EVM, producing the state
/// and receipts the next block will commit to.
///
/// A transaction REVM refuses to execute is *skipped*, not included: a wrong
/// nonce or a sender who can no longer afford the gas means it was never valid
/// to mine. Breeja validates everything before it reaches the mempool, so the
/// only refusals left are races — a nonce that another transaction already used.
pub fn execute_block(
    world: WorldState,
    block_hashes: HashMap<u64, B256>,
    batch: &[PendingTransaction],
    block: &BlockContext,
) -> Execution {
    let mut db = BreejaDb::new(world, block_hashes);
    let mut transactions = Vec::new();
    let mut receipts = Vec::new();
    let mut gas_used = 0u64;

    for tx in batch {
        let outcome = match execute_transaction(&mut db, block, tx) {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(tx = %tx.hash, %error, "dropping transaction that no longer applies");
                continue;
            }
        };

        let index = transactions.len() as u64;
        gas_used += outcome.gas_used;

        transactions.push(Transaction {
            hash: tx.hash,
            // The block hash is unknown until the block is sealed, so it is
            // stamped in afterwards.
            block_hash: B256::ZERO,
            block_number: block.number,
            transaction_index: index,
            chain_id: tx.chain_id,
            from: tx.from,
            to: tx.to,
            value: tx.value,
            nonce: tx.nonce,
            gas: tx.gas,
            gas_price: tx.gas_price,
            input: Default::default(),
            v: tx.v,
            r: tx.r,
            s: tx.s,
        });

        receipts.push(TransactionReceipt {
            transaction_hash: tx.hash,
            transaction_index: index,
            block_hash: B256::ZERO,
            block_number: block.number,
            from: tx.from,
            to: tx.to,
            gas_used: outcome.gas_used,
            cummulative_gas_used: gas_used,
            effective_gas_price: tx.gas_price,
            status: outcome.success as u8,
        });
    }

    let mut world = db.into_world();

    // The miner's reward is paid after the transactions, so a miner cannot spend
    // it inside the very block that grants it.
    if let Err(error) = pay_block_reward(&mut world.accounts, block.miner) {
        tracing::error!(%error, "failed to pay block reward");
    }

    let tx_hashes: Vec<B256> = transactions.iter().map(|tx| tx.hash).collect();
    let receipt_hashes: Vec<B256> = receipts.iter().map(|r| r.transaction_hash).collect();

    Execution {
        transaction_root: hash_list_root(&tx_hashes),
        receipts_root: hash_list_root(&receipt_hashes),
        state_root: primitives::node::state_root_of(&world.accounts),
        world,
        transactions,
        receipts,
        gas_used,
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::U256;
    use primitives::constants::{CHAIN_ID, DEV_ADDRESS, DEV_ADDRESS_1, GAS_PRICE, TRANSFER_GAS};

    use super::*;
    use crate::evm::{InvalidTransaction, TxOutcome};

    fn one_eth() -> U256 {
        U256::from(10u64).pow(U256::from(18))
    }

    fn miner() -> Address {
        Address::repeat_byte(0x99)
    }

    fn block() -> BlockContext {
        BlockContext {
            number: 1,
            timestamp: 1,
            miner: miner(),
            difficulty: 1,
            parent_hash: B256::repeat_byte(0x01),
        }
    }

    fn funded(balance: U256) -> WorldState {
        let mut accounts = HashMap::new();
        accounts.insert(DEV_ADDRESS, Account::new(balance));
        WorldState::from_accounts(accounts)
    }

    fn transfer(value: U256, nonce: u64) -> PendingTransaction {
        PendingTransaction {
            hash: B256::repeat_byte(0xab),
            from: DEV_ADDRESS,
            to: DEV_ADDRESS_1,
            value,
            nonce,
            gas: TRANSFER_GAS,
            gas_price: GAS_PRICE,
            chain_id: CHAIN_ID,
            v: 0,
            r: U256::ZERO,
            s: U256::ZERO,
        }
    }

    /// Executes one transaction against `world` the way a block would, returning
    /// the outcome and the state it left behind.
    ///
    /// These tests used to call `apply_transfer` on a bare account map. That
    /// function is gone — REVM performs the transfer now — so they go through
    /// the EVM instead. What each test asserts is unchanged.
    fn run(
        world: WorldState,
        tx: &PendingTransaction,
    ) -> (Result<TxOutcome, InvalidTransaction>, WorldState) {
        let mut db = BreejaDb::new(world, HashMap::new());
        let outcome = execute_transaction(&mut db, &block(), tx);
        (outcome, db.into_world())
    }

    #[test]
    fn a_transfer_through_revm_costs_exactly_21000_gas() {
        let start = one_eth() * U256::from(10u64);
        let (outcome, world) = run(funded(start), &transfer(one_eth(), 0));
        let outcome = outcome.expect("a funded transfer is valid");

        // Parity with the constant Breeja used to charge. If this drifts, the
        // TxEnv or CfgEnv is wrong, not the EVM.
        assert!(outcome.success);
        assert_eq!(outcome.gas_used, 21_000);

        let fee = U256::from(21_000u64) * U256::from(GAS_PRICE);
        // REVM paid the beneficiary exactly once — the manual credit is gone.
        assert_eq!(world.account(&miner()).balance, fee);
        assert_eq!(world.account(&DEV_ADDRESS_1).balance, one_eth());
        assert_eq!(world.account(&DEV_ADDRESS).balance, start - one_eth() - fee);
        // REVM bumped the nonce exactly once — the manual bump is gone.
        assert_eq!(world.account(&DEV_ADDRESS).nonce, 1);
    }

    #[test]
    fn a_transfer_moves_value_charges_a_fee_and_bumps_the_nonce() {
        let fee = U256::from(TRANSFER_GAS) * U256::from(GAS_PRICE);

        let (outcome, world) = run(funded(one_eth() * U256::from(10u64)), &transfer(one_eth(), 0));
        assert_eq!(outcome.unwrap().gas_used, TRANSFER_GAS);

        let sender = world.account(&DEV_ADDRESS);
        assert_eq!(
            sender.balance,
            one_eth() * U256::from(10u64) - one_eth() - fee
        );
        assert_eq!(sender.nonce, 1);
        assert_eq!(world.account(&DEV_ADDRESS_1).balance, one_eth());
        assert_eq!(world.account(&miner()).balance, fee);
    }

    #[test]
    fn no_eth_is_created_or_destroyed_by_a_transfer() {
        let start = one_eth() * U256::from(10u64);

        let (outcome, world) = run(funded(start), &transfer(one_eth(), 0));
        outcome.unwrap();

        let total: U256 = world
            .accounts
            .values()
            .map(|a| a.balance)
            .fold(U256::ZERO, |a, b| a + b);
        assert_eq!(total, start);
    }

    #[test]
    fn a_transfer_the_sender_cannot_afford_is_rejected() {
        // Enough for the value but not the fee on top.
        let (outcome, world) = run(funded(one_eth()), &transfer(one_eth(), 0));
        assert!(outcome.is_err());
        // Rejected means untouched: no fee, no nonce bump.
        assert_eq!(world.account(&DEV_ADDRESS).balance, one_eth());
        assert_eq!(world.account(&DEV_ADDRESS).nonce, 0);
    }

    #[test]
    fn a_transfer_with_the_wrong_nonce_is_rejected() {
        let (outcome, _) = run(funded(one_eth() * U256::from(10u64)), &transfer(one_eth(), 7));
        assert!(outcome.is_err());
    }

    #[test]
    fn executing_a_block_pays_the_reward_and_records_receipts() {
        let world = funded(one_eth() * U256::from(10u64));
        let batch = vec![transfer(one_eth(), 0)];

        let execution = execute_block(world, HashMap::new(), &batch, &block());

        assert_eq!(execution.transactions.len(), 1);
        assert_eq!(execution.receipts.len(), 1);
        assert_eq!(execution.receipts[0].status, 1);
        assert_eq!(execution.gas_used, TRANSFER_GAS);
        assert_eq!(execution.receipts[0].cummulative_gas_used, TRANSFER_GAS);

        let fee = U256::from(TRANSFER_GAS) * U256::from(GAS_PRICE);
        assert_eq!(execution.world.accounts[&miner()].balance, BLOCK_REWARD + fee);

        // Non-empty roots, because this block committed to a transaction.
        assert_ne!(execution.transaction_root, B256::ZERO);
        assert_ne!(execution.state_root, B256::ZERO);
    }

    #[test]
    fn an_empty_block_still_pays_the_miner_but_commits_to_no_transactions() {
        let execution = execute_block(funded(one_eth()), HashMap::new(), &[], &block());

        assert!(execution.transactions.is_empty());
        assert_eq!(execution.gas_used, 0);
        assert_eq!(execution.transaction_root, B256::ZERO);
        assert_eq!(execution.world.accounts[&miner()].balance, BLOCK_REWARD);
    }

    #[test]
    fn a_transaction_that_no_longer_applies_is_skipped_not_mined() {
        let world = funded(one_eth() * U256::from(10u64));
        // Two transactions claiming the same nonce: only the first can apply.
        let batch = vec![transfer(one_eth(), 0), transfer(one_eth(), 0)];

        let execution = execute_block(world, HashMap::new(), &batch, &block());
        assert_eq!(execution.transactions.len(), 1);
    }
}
