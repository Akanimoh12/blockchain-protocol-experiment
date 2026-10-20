use alloy::{
    primitives::{Address, B256, Bytes, U256},
    sol_types::{Panic, Revert, SolError},
};
use jsonrpsee::{
    core::RpcResult,
    types::{ErrorObjectOwned, Params, error::ErrorCode},
};
use node::evm::{
    BlockContext, BreejaDb, EstimateError, SimulatedCall, TxOutcome, estimate_gas, simulate,
};
use primitives::{
    constants::{BLOCK_GAS_LIMIT, CHAIN_ID},
    node::BlockState,
    params::{BlockTag, CallRequest, LogFilter},
    response::{
        BlockResponse, LogResponse, TransactionReceiptResponse, TransactionResponse, data_bytes,
        data_hash, quantity,
    },
};
use serde_json::Value;

use crate::RPCContext;

/// JSON-RPC error code Ethereum clients use for "your request was understood but
/// is not valid", as opposed to a malformed one.
const INVALID_INPUT: i32 = -32000;

/// Turns an internal error into a JSON-RPC error object the client can read.
fn invalid_input(error: impl std::fmt::Display) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(INVALID_INPUT, error.to_string(), None::<()>)
}

fn invalid_params(error: impl std::fmt::Display) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(
        ErrorCode::InvalidParams.code(),
        error.to_string(),
        None::<()>,
    )
}

/// Parses a 32-byte hash parameter.
fn parse_hash(raw: &str) -> Result<B256, ErrorObjectOwned> {
    raw.parse::<B256>()
        .map_err(|e| invalid_params(format!("invalid 32-byte hash {raw:?}: {e}")))
}

/// Resolves a block tag against the chain head, erroring on a block that does not
/// exist rather than silently substituting the latest one.
fn resolve_tag(state: &BlockState, tag: &BlockTag) -> Result<u64, ErrorObjectOwned> {
    tag.resolve(state.get_block_number()).map_err(invalid_input)
}


/// The chain's id, used by wallets for EIP-155 replay protection.
pub fn chain_id_handler() -> String {
    quantity(CHAIN_ID)
}


/// The number of the chain head. A freshly started node sits at `0x0`, the genesis
/// block, and climbs from there as the miner seals blocks.
pub fn block_number_handler(context: &RPCContext) -> String {
    quantity(context.chain.read().get_block_number())
}


/// Renders a block, inlining full transaction objects if the caller asked for them.
fn render_block(state: &BlockState, number: u64, full_transactions: bool) -> Value {
    let Some(block) = state.get_block(number) else {
        return Value::Null;
    };

    let transactions: Vec<Value> = if full_transactions {
        block
            .transactions
            .iter()
            .filter_map(|hash| state.get_transaction(hash))
            .map(|tx| {
                serde_json::to_value(TransactionResponse::from(tx))
                    .expect("a transaction response always serializes")
            })
            .collect()
    } else {
        block
            .transactions
            .iter()
            .map(|hash| Value::String(data_hash(hash)))
            .collect()
    };

    serde_json::to_value(BlockResponse::from_block(block, transactions))
        .expect("a block response always serializes")
}

pub fn get_block_by_number_handler(params: Params<'_>, context: &RPCContext) -> RpcResult<Value> {
    // Ethereum's optional trailing parameters have to be read one at a time: a
    // client may send just the block, or the block and the `fullTransactions`
    // flag, and both are valid.
    let mut sequence = params.sequence();
    let tag: BlockTag = sequence.next()?;
    let full_transactions = sequence.optional_next::<bool>()?.unwrap_or(false);

    let state = context.chain.read();
    let number = resolve_tag(&state, &tag)?;
    Ok(render_block(&state, number, full_transactions))
}

pub fn get_block_by_hash_handler(params: Params<'_>, context: &RPCContext) -> RpcResult<Value> {
    let mut sequence = params.sequence();
    let hash = parse_hash(&sequence.next::<String>()?)?;
    let full_transactions = sequence.optional_next::<bool>()?.unwrap_or(false);

    let state = context.chain.read();
    // An unknown hash is `null`, not an error: the caller asked a legitimate
    // question and the answer is "no such block".
    let Some(number) = state.get_block_by_hash(&hash).map(|block| block.number) else {
        return Ok(Value::Null);
    };
    Ok(render_block(&state, number, full_transactions))
}



pub fn get_transaction_by_hash_handler(
    params: Params<'_>,
    context: &RPCContext,
) -> RpcResult<Value> {
    let hash = parse_hash(&params.one::<String>()?)?;

    let state = context.chain.read();
    match state.get_transaction(&hash) {
        Some(tx) => Ok(serde_json::to_value(TransactionResponse::from(tx))
            .expect("a transaction response always serializes")),
        None => Ok(Value::Null),
    }
}

/// A receipt exists only once a transaction has been mined. A transaction still
/// sitting in the mempool correctly returns `null` here, which is exactly how a
/// client polls for confirmation.
pub fn get_transaction_receipt_handler(
    params: Params<'_>,
    context: &RPCContext,
) -> RpcResult<Value> {
    let hash = parse_hash(&params.one::<String>()?)?;

    let state = context.chain.read();
    match state.get_receipt(&hash) {
        Some(receipt) => Ok(
            serde_json::to_value(TransactionReceiptResponse::from(receipt))
                .expect("a receipt response always serializes"),
        ),
        None => Ok(Value::Null),
    }
}


pub fn get_balance_handler(params: Params<'_>, context: &RPCContext) -> RpcResult<String> {
    let mut sequence = params.sequence();
    let address: Address = sequence.next()?;
    let tag = sequence
        .optional_next::<BlockTag>()?
        .unwrap_or(BlockTag::Latest);

    let state = context.chain.read();
    let balance = match tag {
        // "pending" means "after everything in the mempool lands", which is what a
        // wallet wants before it builds the next transaction.
        BlockTag::Pending => state.get_pending_balance(&address),
        _ => {
            let number = resolve_tag(&state, &tag)?;
            state
                .get_account_at(&address, number)
                .map(|account| account.balance)
                .unwrap_or(U256::ZERO)
        }
    };

    Ok(quantity(balance))
}

/// The account's nonce: how many transactions it has sent.
pub fn get_transaction_count_handler(
    params: Params<'_>,
    context: &RPCContext,
) -> RpcResult<String> {
    let mut sequence = params.sequence();
    let address: Address = sequence.next()?;
    let tag = sequence
        .optional_next::<BlockTag>()?
        .unwrap_or(BlockTag::Latest);

    let state = context.chain.read();
    let nonce = match tag {
        BlockTag::Pending => state.get_pending_nonce(&address),
        _ => {
            let number = resolve_tag(&state, &tag)?;
            state
                .get_account_at(&address, number)
                .map(|account| account.nonce)
                .unwrap_or(0)
        }
    };

    Ok(quantity(nonce))
}


/// Accepts a signed transaction, validates it, and queues it for mining.
///
/// The response is the transaction hash, returned the moment the transaction is
/// *accepted* — not when it is mined. The client then polls
/// `eth_getTransactionReceipt` until a receipt appears, which is the standard
/// Ethereum confirmation flow.
pub fn send_raw_transaction_handler(params: Params<'_>, context: &RPCContext) -> RpcResult<String> {
    let raw: String = params.one()?;
    let raw: Bytes = raw
        .parse()
        .map_err(|e| invalid_params(format!("raw transaction is not valid hex: {e}")))?;

    let hash = context
        .chain
        .submit_raw_transaction(&raw)
        .map_err(invalid_input)?;

    Ok(data_hash(&hash))
}


/// The node's gas price: a constant, because Breeja has no fee market.
///
/// One line, but it is what lets `cast send` and wallets fill in a legacy
/// transaction's price themselves instead of needing it spelled out.
pub fn gas_price_handler(context: &RPCContext) -> String {
    quantity(context.chain.read().gas_price())
}

/// Reads the block tag that trails an address-based query, defaulting to
/// `latest`, and resolves it to a block number.
fn trailing_block(
    sequence: &mut jsonrpsee::types::params::ParamsSequence<'_>,
    state: &BlockState,
) -> Result<u64, ErrorObjectOwned> {
    let tag = sequence
        .optional_next::<BlockTag>()?
        .unwrap_or(BlockTag::Latest);
    resolve_tag(state, &tag)
}

/// The runtime code at an address, `0x` for an account with no code.
pub fn get_code_handler(params: Params<'_>, context: &RPCContext) -> RpcResult<String> {
    let mut sequence = params.sequence();
    let address: Address = sequence.next()?;

    let state = context.chain.read();
    let number = trailing_block(&mut sequence, &state)?;
    let code = state.get_code_at(&address, number).unwrap_or_default();
    Ok(data_bytes(&code))
}

/// One storage slot, always as a full 32-byte word, even when it holds zero.
pub fn get_storage_at_handler(params: Params<'_>, context: &RPCContext) -> RpcResult<String> {
    let mut sequence = params.sequence();
    let address: Address = sequence.next()?;
    let slot: U256 = sequence.next()?;

    let state = context.chain.read();
    let number = trailing_block(&mut sequence, &state)?;
    let value = state
        .get_storage_at(&address, slot, number)
        .unwrap_or_default();
    Ok(data_hash(&B256::from(value)))
}

/// Logs matching a filter, oldest first.
///
/// The filter's block range picks which blocks to walk; `LogFilter::matches`
/// applies the address and topic constraints to every log in them.
pub fn get_logs_handler(params: Params<'_>, context: &RPCContext) -> RpcResult<Value> {
    let filter: LogFilter = params.one()?;
    let state = context.chain.read();

    let (from, to) = match filter.block_hash {
        Some(hash) => {
            if filter.from_block.is_some() || filter.to_block.is_some() {
                return Err(invalid_params(
                    "blockHash cannot be combined with fromBlock or toBlock",
                ));
            }
            // An unknown block has no logs; geth answers the same way.
            let Some(block) = state.get_block_by_hash(&hash) else {
                return Ok(Value::Array(Vec::new()));
            };
            (block.number, block.number)
        }
        None => {
            let from = resolve_tag(&state, &filter.from_block.unwrap_or(BlockTag::Latest))?;
            let to = resolve_tag(&state, &filter.to_block.unwrap_or(BlockTag::Latest))?;
            if from > to {
                return Err(invalid_params(format!(
                    "fromBlock {from} is after toBlock {to}"
                )));
            }
            (from, to)
        }
    };

    let logs: Vec<Value> = (from..=to)
        .filter_map(|number| state.get_block(number))
        .flat_map(|block| block.transactions.iter())
        .filter_map(|hash| state.get_receipt(hash))
        .flat_map(|receipt| receipt.logs.iter())
        .filter(|log| filter.matches(log))
        .map(|log| {
            serde_json::to_value(LogResponse::from(log)).expect("a log response always serializes")
        })
        .collect();

    Ok(Value::Array(logs))
}

/// Error code geth uses for a reverted call, with the revert data attached, so a
/// client can decode the contract's reason.
const EXECUTION_REVERTED: i32 = 3;

/// The human-readable reason inside revert data, if it is in one of Solidity's
/// two standard shapes: `Error(string)`, from `revert("…")` and
/// `require(cond, "…")`, or `Panic(uint256)`, from a failed `assert` or an
/// arithmetic overflow.
fn revert_reason(output: &[u8]) -> Option<String> {
    if let Ok(revert) = Revert::abi_decode(output) {
        return Some(revert.reason);
    }
    Panic::abi_decode(output).ok().map(|panic| panic.to_string())
}

/// Turns a call that ran but failed into the error a client expects.
///
/// A revert's reason goes into the message as well as the data, the way geth
/// reports it: tools like cast show the message, so this is what makes
/// `require(…, "reason")` visible to whoever is calling.
fn execution_failed(outcome: &TxOutcome) -> ErrorObjectOwned {
    if outcome.reverted() {
        let message = match revert_reason(&outcome.output) {
            Some(reason) => format!("execution reverted: {reason}"),
            None => "execution reverted".to_owned(),
        };
        ErrorObjectOwned::owned(EXECUTION_REVERTED, message, Some(data_bytes(&outcome.output)))
    } else {
        invalid_input(format!(
            "execution halted: {}",
            outcome.halt_reason.as_deref().unwrap_or("unknown reason")
        ))
    }
}

/// A private copy of the state at `tag` (default `latest`) and the context of
/// the block that would follow it.
fn simulation_env(
    context: &RPCContext,
    tag: Option<BlockTag>,
) -> Result<(BreejaDb, BlockContext), ErrorObjectOwned> {
    let number = resolve_tag(&context.chain.read(), &tag.unwrap_or(BlockTag::Latest))?;
    context
        .chain
        .simulation_env(number)
        .ok_or_else(|| invalid_input(format!("block {number} does not exist")))
}

/// Maps a JSON-RPC call object onto a simulation. Omitted fields take the
/// defaults Ethereum clients expect: the zero address as sender, no value, and a
/// gas price of zero, so the gas itself costs nothing.
fn simulated_call(
    request: &CallRequest,
    check_balance: bool,
) -> Result<SimulatedCall, ErrorObjectOwned> {
    let gas_price = match request.gas_price {
        Some(price) => u128::try_from(price)
            .map_err(|_| invalid_params(format!("gasPrice {price} does not fit in 128 bits")))?,
        None => 0,
    };
    Ok(SimulatedCall {
        from: request.from.unwrap_or_default(),
        to: request.to,
        value: request.value.unwrap_or_default(),
        data: request.calldata(),
        gas_price,
        check_balance,
    })
}

/// The gas the caller allows, or a whole block's worth if they did not say.
fn gas_cap(request: &CallRequest) -> Result<u64, ErrorObjectOwned> {
    match request.gas {
        Some(gas) => u64::try_from(gas)
            .map_err(|_| invalid_params(format!("gas {gas} does not fit in 64 bits"))),
        None => Ok(BLOCK_GAS_LIMIT),
    }
}

/// Executes a call against the state at a block and returns what it returned,
/// keeping none of its effects.
///
/// That is the only difference from `eth_sendRawTransaction`: the same EVM runs
/// the same code, but the state changes are thrown away instead of mined.
///
/// Neither the nonce nor the balance is checked. A call request carries no
/// nonce, and reading a contract should not require the caller to be able to
/// pay for gas that is never charged.
pub fn call_handler(params: Params<'_>, context: &RPCContext) -> RpcResult<String> {
    let mut sequence = params.sequence();
    let request: CallRequest = sequence.next()?;
    let tag = sequence.optional_next::<BlockTag>()?;

    let (db, block) = simulation_env(context, tag)?;
    let call = simulated_call(&request, false)?;
    let outcome = simulate(&db, &block, &call, gas_cap(&request)?).map_err(invalid_input)?;

    if !outcome.success {
        return Err(execution_failed(&outcome));
    }
    Ok(data_bytes(&outcome.output))
}

/// Estimates the gas a transaction would need by executing it.
///
/// Once code can run, the cost is only knowable by running it, so this searches
/// for the smallest gas limit that succeeds (see `node::evm::estimate_gas`). A
/// plain transfer still comes out at exactly 21,000.
///
/// Unlike `eth_call`, the sender must be able to afford the transaction: an
/// estimate for something that could never be sent is a client bug worth
/// surfacing, not a number worth returning.
pub fn estimate_gas_handler(params: Params<'_>, context: &RPCContext) -> RpcResult<String> {
    let mut sequence = params.sequence();
    let request: CallRequest = sequence.next()?;
    let tag = sequence.optional_next::<BlockTag>()?;

    let (db, block) = simulation_env(context, tag)?;
    let call = simulated_call(&request, true)?;

    match estimate_gas(&db, &block, &call, gas_cap(&request)?) {
        Ok(gas) => Ok(quantity(gas)),
        Err(EstimateError::Invalid(error)) => Err(invalid_input(error)),
        Err(EstimateError::Failed(outcome)) => Err(execution_failed(&outcome)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reverted_with(output: Bytes) -> TxOutcome {
        TxOutcome {
            success: false,
            gas_used: 0,
            created_address: None,
            logs: Vec::new(),
            output,
            halt_reason: None,
        }
    }

    #[test]
    fn a_solidity_revert_reason_is_put_in_the_message() {
        // What `revert("Counter: nope")` returns.
        let data = Bytes::from(Revert::from("Counter: nope").abi_encode());
        let error = execution_failed(&reverted_with(data.clone()));

        assert_eq!(error.code(), EXECUTION_REVERTED);
        assert_eq!(error.message(), "execution reverted: Counter: nope");
        // The raw data is still attached for clients that decode it themselves.
        assert_eq!(
            error.data().map(|d| d.get().to_owned()),
            Some(format!("\"{}\"", data_bytes(&data)))
        );
    }

    #[test]
    fn a_panic_is_described_and_unknown_data_is_left_raw() {
        // What `assert(false)` returns: Panic(0x01).
        let panic = Bytes::from(Panic::from(1u64).abi_encode());
        let message = execution_failed(&reverted_with(panic)).message().to_owned();
        assert!(message.starts_with("execution reverted: panic: assert"), "got: {message}");

        // `REVERT(0, 0)` and custom errors carry nothing we can read.
        let bare = execution_failed(&reverted_with(Bytes::new()));
        assert_eq!(bare.message(), "execution reverted");
        let custom = execution_failed(&reverted_with(Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef])));
        assert_eq!(custom.message(), "execution reverted");
    }
}
