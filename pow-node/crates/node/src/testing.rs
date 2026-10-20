use alloy::{
    consensus::{SignableTransaction, TxLegacy},
    eips::Encodable2718,
    primitives::{Address, Bytes, TxKind, U256},
    signers::{SignerSync, local::PrivateKeySigner},
};
use primitives::constants::{CHAIN_ID, GAS_PRICE, TRANSFER_GAS};

/// The private key of the first pre-funded development account
/// (`0xf39F...2266`). This is the standard Anvil/Hardhat test key: it is public
/// knowledge and must never hold real funds.
pub const DEV_PRIVATE_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

/// The second development account's key (`0x7099...79C8`).
pub const DEV_PRIVATE_KEY_1: &str =
    "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";

/// Signs an ETH transfer and encodes it the way `eth_sendRawTransaction` expects.
pub fn sign_transfer(
    private_key: &str,
    to: Address,
    value: U256,
    nonce: u64,
) -> Result<Bytes, anyhow::Error> {
    sign_transfer_with(private_key, to, value, nonce, GAS_PRICE, TRANSFER_GAS)
}

/// Signs a transfer with explicit gas parameters, for exercising the node's
/// validation rules.
pub fn sign_transfer_with(
    private_key: &str,
    to: Address,
    value: U256,
    nonce: u64,
    gas_price: u128,
    gas_limit: u64,
) -> Result<Bytes, anyhow::Error> {
    sign_tx(
        private_key,
        TxKind::Call(to),
        value,
        Bytes::new(),
        nonce,
        gas_price,
        gas_limit,
    )
}

/// Signs a contract deployment: no recipient, and `initcode` as the input. The
/// contract will live at `address_of(private_key).create(nonce)`.
pub fn sign_deploy(
    private_key: &str,
    initcode: Bytes,
    nonce: u64,
    gas_limit: u64,
) -> Result<Bytes, anyhow::Error> {
    sign_tx(
        private_key,
        TxKind::Create,
        U256::ZERO,
        initcode,
        nonce,
        GAS_PRICE,
        gas_limit,
    )
}

/// Signs a call to a contract with `calldata` and no value.
pub fn sign_call(
    private_key: &str,
    to: Address,
    calldata: Bytes,
    nonce: u64,
    gas_limit: u64,
) -> Result<Bytes, anyhow::Error> {
    sign_tx(
        private_key,
        TxKind::Call(to),
        U256::ZERO,
        calldata,
        nonce,
        GAS_PRICE,
        gas_limit,
    )
}

/// Signs any legacy transaction and encodes it the way `eth_sendRawTransaction`
/// expects.
pub fn sign_tx(
    private_key: &str,
    to: TxKind,
    value: U256,
    input: Bytes,
    nonce: u64,
    gas_price: u128,
    gas_limit: u64,
) -> Result<Bytes, anyhow::Error> {
    let signer: PrivateKeySigner = private_key.parse()?;

    // 1. The unsigned transaction.
    let tx = TxLegacy {
        chain_id: Some(CHAIN_ID),
        nonce,
        gas_price,
        gas_limit,
        to,
        value,
        input,
    };

    // 2 and 3. Sign the signature hash, which commits to every field above.
    let signature = signer.sign_hash_sync(&tx.signature_hash())?;

    // 4. Attach the signature and encode for the wire.
    let signed = tx.into_signed(signature);
    let mut encoded = Vec::new();
    signed.encode_2718(&mut encoded);

    Ok(Bytes::from(encoded))
}

/// The address a private key controls.
pub fn address_of(private_key: &str) -> Result<Address, anyhow::Error> {
    let signer: PrivateKeySigner = private_key.parse()?;
    Ok(signer.address())
}

/// One ETH, in wei.
pub fn one_eth() -> U256 {
    U256::from(10u64).pow(U256::from(18))
}

// Hand-assembled test contracts. Each initcode is the same 12-byte prologue —
// `PUSH1 len PUSH1 0x0c PUSH1 0 CODECOPY PUSH1 len PUSH1 0 RETURN`, which copies
// the bytes after it into memory and returns them as the runtime code — followed
// by that runtime. Read any of them with `cast disassemble <hex>`.

/// Runtime: `sstore(0, 42)`, then `LOG0` with no data, then `STOP`.
pub const STORE42_INITCODE: &str = "0x600b600c600039600b6000f3602a60005560006000a000";
/// What `STORE42_INITCODE` leaves on chain.
pub const STORE42_RUNTIME: &str = "0x602a60005560006000a000";

/// Runtime: `REVERT(0, 0)` — every call to it fails.
pub const REVERTER_INITCODE: &str = "0x6005600c60003960056000f360006000fd";

/// Runtime: return the 32-byte word `42`.
pub const RETURN42_INITCODE: &str = "0x600a600c600039600a6000f3602a60005260206000f3";

/// Runtime: `JUMPDEST PUSH1 0 JUMP` — loops until it runs out of gas, so a call
/// burns its entire gas limit.
pub const LOOPER_INITCODE: &str = "0x6004600c60003960046000f35b600056";

/// Runtime: `LOG1` with topic [`PING_TOPIC`] and no data, then `STOP`. Unlike
/// STORE42's `LOG0`, this log has a topic to filter on.
pub const PINGER_INITCODE: &str = "0x6027600c60003960276000f37fca6e822df923f741dfe968d15d80a18abd25bd1e748bcb9ad81fea5bbb7386af60006000a100";

/// `keccak256("Ping()")`, the topic PINGER emits — what Solidity would use as
/// the signature topic of an `event Ping()`.
pub const PING_TOPIC: &str = "0xca6e822df923f741dfe968d15d80a18abd25bd1e748bcb9ad81fea5bbb7386af";
