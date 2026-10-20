//! End-to-end contract tests: deploy, call, revert — all mined by the real miner.

use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, Bloom, BloomInput, Bytes, U256};
use node::{
    Chain, ChainConfig, miner,
    testing::{
        DEV_PRIVATE_KEY, DEV_PRIVATE_KEY_1, LOOPER_INITCODE, PING_TOPIC, PINGER_INITCODE,
        REVERTER_INITCODE, STORE42_INITCODE, STORE42_RUNTIME, address_of, one_eth, sign_call,
        sign_deploy, sign_transfer_with,
    },
};
use primitives::{
    constants::{BLOCK_GAS_LIMIT, BLOCK_REWARD, GAS_PRICE},
    node::{BreejaGenesis, TransactionReceipt, logs_bloom},
};

/// A chain that mines almost instantly, so tests do not spend real time hashing.
fn test_chain(funded: &[(Address, U256)]) -> Chain {
    let config = ChainConfig {
        difficulty: 4,
        target_block_time: Duration::from_millis(50),
        miner: Address::repeat_byte(0x99),
    };
    Chain::new(
        BreejaGenesis {
            accounts: funded.to_vec(),
        },
        config,
    )
}

/// Blocks until `predicate` holds, or fails the test after `timeout`.
fn wait_for(timeout: Duration, label: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out after {timeout:?} waiting for {label}");
}

fn hex(raw: &str) -> Bytes {
    raw.parse().expect("valid hex")
}

/// Submits every transaction, mines until all of them have receipts, and
/// returns those receipts in submission order.
fn mine(chain: &Chain, raws: &[Bytes]) -> Vec<TransactionReceipt> {
    let hashes: Vec<B256> = raws
        .iter()
        .map(|raw| chain.submit_raw_transaction(raw).unwrap())
        .collect();

    let handle = miner::spawn(chain.clone());
    wait_for(Duration::from_secs(10), "every transaction to be mined", || {
        hashes.iter().all(|h| chain.read().get_receipt(h).is_some())
    });
    chain.shutdown();
    handle.join().unwrap();

    let state = chain.read();
    hashes
        .iter()
        .map(|h| state.get_receipt(h).unwrap().clone())
        .collect()
}

#[test]
fn deploying_a_contract_records_its_address_and_code() {
    let alice = address_of(DEV_PRIVATE_KEY).unwrap();
    let chain = test_chain(&[(alice, one_eth())]);

    let deploy = sign_deploy(DEV_PRIVATE_KEY, hex(STORE42_INITCODE), 0, 100_000).unwrap();
    let receipt = &mine(&chain, &[deploy])[0];

    // A contract's address is derived from its deployer and the deployer's
    // nonce, so it is known before the transaction is even sent.
    let expected = alice.create(0);
    assert_eq!(receipt.status, 1);
    assert_eq!(receipt.contract_address, Some(expected));
    assert_eq!(receipt.to, None, "a deployment has no recipient");
    // Matches what the REVM-backed `anvil` charges for the same initcode.
    assert_eq!(receipt.gas_used, 55_522);

    let state = chain.read();
    let head = state.get_block_number();
    // The *runtime* is committed, not the initcode that produced it.
    assert_eq!(
        state.get_code_at(&expected, head).unwrap(),
        hex(STORE42_RUNTIME)
    );
    assert_ne!(state.get_account(&expected).code_hash, B256::ZERO);

    let tx = state.get_transaction(&receipt.transaction_hash).unwrap();
    assert_eq!(tx.to, None);
    assert_eq!(tx.input, hex(STORE42_INITCODE));
}

#[test]
fn calling_a_contract_writes_storage_and_emits_a_log() {
    let alice = address_of(DEV_PRIVATE_KEY).unwrap();
    let chain = test_chain(&[(alice, one_eth())]);
    let contract = alice.create(0);

    let receipts = mine(
        &chain,
        &[
            sign_deploy(DEV_PRIVATE_KEY, hex(STORE42_INITCODE), 0, 100_000).unwrap(),
            sign_call(DEV_PRIVATE_KEY, contract, Bytes::new(), 1, 100_000).unwrap(),
        ],
    );
    let call = &receipts[1];

    assert_eq!(call.status, 1);
    assert_eq!(call.contract_address, None, "a call deploys nothing");
    // A fresh SSTORE (0 -> 42) plus LOG0, as measured on anvil.
    assert_eq!(call.gas_used, 43_487);

    // The log: emitted by the contract, LOG0 so no topics, and no data.
    assert_eq!(call.logs.len(), 1);
    let log = &call.logs[0];
    assert_eq!(log.address, contract);
    assert!(log.topics.is_empty());
    assert!(log.data.is_empty());
    assert_eq!(log.transaction_hash, call.transaction_hash);
    assert_eq!(log.block_number, call.block_number);
    // The block hash is only known after sealing; it must have been stamped in.
    assert_eq!(log.block_hash, call.block_hash);
    assert_ne!(log.block_hash, B256::ZERO);

    let state = chain.read();
    let head = state.get_block_number();
    assert_eq!(
        state.get_storage_at(&contract, U256::ZERO, head).unwrap(),
        U256::from(42u64)
    );
    // Before the call's block, the slot was still empty: history is preserved.
    assert_eq!(
        state
            .get_storage_at(&contract, U256::ZERO, call.block_number - 1)
            .unwrap_or_default(),
        U256::ZERO
    );
}

#[test]
fn a_reverting_transaction_is_mined_charged_and_advances_the_nonce() {
    let alice = address_of(DEV_PRIVATE_KEY).unwrap();
    let start = one_eth();
    let chain = test_chain(&[(alice, start)]);
    let reverter = alice.create(0);

    let receipts = mine(
        &chain,
        &[
            sign_deploy(DEV_PRIVATE_KEY, hex(REVERTER_INITCODE), 0, 100_000).unwrap(),
            sign_call(DEV_PRIVATE_KEY, reverter, Bytes::new(), 1, 50_000).unwrap(),
        ],
    );
    let (deploy, call) = (&receipts[0], &receipts[1]);
    assert_eq!(deploy.status, 1);

    // Failed, yet mined: it has a receipt, in a real block, with status 0.
    assert_eq!(call.status, 0);
    assert!(call.logs.is_empty(), "a revert undoes its logs");
    // It burned more than the intrinsic cost: the code ran before reverting.
    assert!(call.gas_used > 21_000);

    let state = chain.read();
    let block = state.get_block_by_hash(&call.block_hash).unwrap();
    assert!(block.transactions.contains(&call.transaction_hash));

    // The sender paid for both transactions' gas, and both nonces were used.
    let alice_now = state.get_account(&alice);
    let gas = U256::from(deploy.gas_used + call.gas_used);
    assert_eq!(alice_now.balance, start - gas * U256::from(GAS_PRICE));
    assert_eq!(alice_now.nonce, 2);
}

#[test]
fn a_deployment_whose_constructor_reverts_creates_nothing() {
    let alice = address_of(DEV_PRIVATE_KEY).unwrap();
    let chain = test_chain(&[(alice, one_eth())]);

    // Initcode that is just `REVERT(0, 0)`: the constructor itself fails.
    let deploy = sign_deploy(DEV_PRIVATE_KEY, hex("0x60006000fd"), 0, 100_000).unwrap();
    let receipt = &mine(&chain, &[deploy])[0];

    assert_eq!(receipt.status, 0);
    assert_eq!(receipt.contract_address, None);

    let state = chain.read();
    let head = state.get_block_number();
    assert!(state.get_code_at(&alice.create(0), head).unwrap().is_empty());
    assert_eq!(state.get_account(&alice).nonce, 1);
}

#[test]
fn contracts_and_reverts_neither_create_nor_destroy_eth() {
    let alice = address_of(DEV_PRIVATE_KEY).unwrap();
    let start = one_eth() * U256::from(10u64);
    let chain = test_chain(&[(alice, start)]);
    let store = alice.create(0);
    let reverter = alice.create(1);

    mine(
        &chain,
        &[
            sign_deploy(DEV_PRIVATE_KEY, hex(STORE42_INITCODE), 0, 100_000).unwrap(),
            sign_deploy(DEV_PRIVATE_KEY, hex(REVERTER_INITCODE), 1, 100_000).unwrap(),
            sign_call(DEV_PRIVATE_KEY, store, Bytes::new(), 2, 100_000).unwrap(),
            sign_call(DEV_PRIVATE_KEY, reverter, Bytes::new(), 3, 50_000).unwrap(),
        ],
    );

    let state = chain.read();
    let minted = BLOCK_REWARD * U256::from(state.get_block_number());
    let total = [alice, store, reverter, chain.config().miner]
        .iter()
        .map(|a| state.get_account(a).balance)
        .fold(U256::ZERO, |acc, b| acc + b);
    assert_eq!(total, start + minted);
}

#[test]
fn a_block_never_exceeds_the_gas_limit_with_mixed_costs() {
    let alice = address_of(DEV_PRIVATE_KEY).unwrap();
    let bob = address_of(DEV_PRIVATE_KEY_1).unwrap();
    let chain = test_chain(&[(alice, one_eth()), (bob, one_eth())]);
    let looper = alice.create(0);

    // Costs from 21,000 to 1,000,000 gas, 4,173,000 in all against a 3,141,592
    // limit, all queued before the miner starts: it cannot fit them in one block.
    let mut raws = vec![sign_deploy(DEV_PRIVATE_KEY, hex(LOOPER_INITCODE), 0, 100_000).unwrap()];
    for nonce in 1..=4 {
        raws.push(sign_call(DEV_PRIVATE_KEY, looper, Bytes::new(), nonce, 1_000_000).unwrap());
    }
    for nonce in 0..3 {
        raws.push(sign_transfer_with(
            DEV_PRIVATE_KEY_1,
            alice,
            U256::from(1u64),
            nonce,
            GAS_PRICE,
            21_000,
        )
        .unwrap());
    }
    let receipts = mine(&chain, &raws);

    let state = chain.read();
    for number in 1..=state.get_block_number() {
        let block = state.get_block(number).unwrap();
        assert!(
            block.gas_used <= BLOCK_GAS_LIMIT,
            "block {number} used {} gas",
            block.gas_used
        );
        // A block's gas used is exactly the sum of its receipts'.
        let summed: u64 = block
            .transactions
            .iter()
            .map(|h| state.get_receipt(h).unwrap().gas_used)
            .sum();
        assert_eq!(block.gas_used, summed);
    }

    // Each loop burned its full limit, so the four could not share one block.
    let calls = &receipts[1..=4];
    assert!(calls.iter().all(|r| r.status == 0 && r.gas_used == 1_000_000));
    let first = calls.first().unwrap().block_number;
    let last = calls.last().unwrap().block_number;
    assert!(last > first, "four 1M-gas calls cannot fit in one block");
    // Everything was eventually mined, nothing was dropped.
    assert_eq!(state.get_account(&alice).nonce, 5);
    assert_eq!(state.get_account(&bob).nonce, 3);
}

#[test]
fn a_blocks_logs_bloom_matches_the_logs_in_it() {
    let alice = address_of(DEV_PRIVATE_KEY).unwrap();
    let chain = test_chain(&[(alice, one_eth())]);
    let pinger = alice.create(0);
    let ping: B256 = PING_TOPIC.parse().unwrap();

    let receipts = mine(
        &chain,
        &[
            sign_deploy(DEV_PRIVATE_KEY, hex(PINGER_INITCODE), 0, 100_000).unwrap(),
            sign_call(DEV_PRIVATE_KEY, pinger, Bytes::new(), 1, 100_000).unwrap(),
        ],
    );
    let call = &receipts[1];
    assert_eq!(call.logs.len(), 1);

    let state = chain.read();
    let block = state.get_block_by_hash(&call.block_hash).unwrap();

    // Non-zero, and exactly the bloom of the logs this block's receipts hold.
    assert_ne!(block.logs_bloom, Bloom::ZERO);
    let in_block = block
        .transactions
        .iter()
        .flat_map(|h| state.get_receipt(h).unwrap().logs.iter());
    assert_eq!(block.logs_bloom, logs_bloom(in_block));
    assert!(block.logs_bloom.contains_input(BloomInput::Raw(pinger.as_slice())));
    assert!(block.logs_bloom.contains_input(BloomInput::Raw(ping.as_slice())));

    // The bloom is part of the hashed header, so the proof of work covers it.
    node::pow::verify(&block.header(), &block.hash, block.difficulty).unwrap();

    // Every block with no logs — genesis, and any empty block the miner sealed
    // while waiting — has an empty bloom.
    for number in 0..=state.get_block_number() {
        let other = state.get_block(number).unwrap();
        if other.hash != block.hash {
            assert_eq!(other.logs_bloom, Bloom::ZERO, "block {number}");
        }
    }
}

#[test]
fn the_mempool_admits_contracts_but_still_enforces_gas_rules() {
    let alice = address_of(DEV_PRIVATE_KEY).unwrap();
    let chain = test_chain(&[(alice, one_eth() * U256::from(10u64))]);

    // Below the intrinsic cost of *this* transaction: 21,000 would have been
    // enough for a transfer, but not for a deployment (+32,000 and calldata).
    let starved = sign_deploy(DEV_PRIVATE_KEY, hex(STORE42_INITCODE), 0, 21_000).unwrap();
    let error = chain.submit_raw_transaction(&starved).unwrap_err();
    assert!(error.to_string().contains("intrinsic"), "got: {error}");

    // A gas limit no block could ever hold.
    let oversized = sign_transfer_with(
        DEV_PRIVATE_KEY,
        Address::repeat_byte(0x22),
        U256::ZERO,
        0,
        GAS_PRICE,
        BLOCK_GAS_LIMIT + 1,
    )
    .unwrap();
    let error = chain.submit_raw_transaction(&oversized).unwrap_err();
    assert!(error.to_string().contains("block gas limit"), "got: {error}");

    // Deployments and calls with calldata are now welcome.
    let deploy = sign_deploy(DEV_PRIVATE_KEY, hex(STORE42_INITCODE), 0, 100_000).unwrap();
    chain.submit_raw_transaction(&deploy).unwrap();
    let call = sign_call(
        DEV_PRIVATE_KEY,
        alice.create(0),
        hex("0xdeadbeef"),
        1,
        100_000,
    )
    .unwrap();
    chain.submit_raw_transaction(&call).unwrap();
    assert_eq!(chain.read().mempool_len(), 2);
}
