#!/usr/bin/env bash
#
# Deploys a Solidity contract to a Breeja node and calls it, using only standard
# Ethereum tooling (forge and cast). Every step checks its result and the script
# exits non-zero on the first surprise.
#
#   ./demo/demo.sh                  # uses a node on 127.0.0.1:8545, or starts one
#   ETH_RPC_URL=http://host:port ./demo/demo.sh
#
# Needs: foundry (forge, cast) and jq. forge downloads solc 0.8.30 on first run.

set -euo pipefail
cd "$(dirname "$0")"

export ETH_RPC_URL="${ETH_RPC_URL:-http://127.0.0.1:8545}"
# Anvil/Hardhat development account 0: pre-funded by Breeja's genesis. Its key is
# public knowledge; never use it for anything real.
PK=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
ME=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
ok()   { printf '   \033[32m✓\033[0m %s\n' "$*"; }
die()  { printf '   \033[31m✗ %s\033[0m\n' "$*"; exit 1; }
expect() { # expect <actual> <wanted> <label>
    [ "$1" = "$2" ] && ok "$3: $1" || die "$3: expected $2, got $1"
}

# --- a node to talk to ---------------------------------------------------------

if ! cast chain-id >/dev/null 2>&1; then
    step "No node at $ETH_RPC_URL; starting one with --dev"
    (cd .. && cargo build --release --quiet)
    ../target/release/breeja --dev --rpc-address "${ETH_RPC_URL#http://}" > node.log 2>&1 &
    NODE_PID=$!
    trap 'kill $NODE_PID 2>/dev/null || true' EXIT
    for _ in $(seq 1 50); do cast chain-id >/dev/null 2>&1 && break; sleep 0.2; done
    cast chain-id >/dev/null 2>&1 || die "node did not start; see demo/node.log"
    ok "node started (log: demo/node.log)"
fi

step "The chain"
expect "$(cast chain-id)" "1" "chain id"
expect "$(cast gas-price)" "1000000000" "gas price (wei)"
ok "head block: $(cast block-number)"

# --- deploy --------------------------------------------------------------------

step "Deploy Counter.sol (compiled for Shanghai)"
DEPLOY=$(forge create Counter.sol:Counter --legacy --broadcast \
    --private-key "$PK" --rpc-url "$ETH_RPC_URL" --json)
COUNTER=$(echo "$DEPLOY" | jq -r .deployedTo)
DEPLOY_TX=$(echo "$DEPLOY" | jq -r .transactionHash)
RECEIPT=$(cast receipt "$DEPLOY_TX" --json)
expect "$(echo "$RECEIPT" | jq -r .status)" "0x1" "deployment status"
expect "$(echo "$RECEIPT" | jq -r .contractAddress)" "$(echo "$COUNTER" | tr 'A-F' 'a-f')" "contractAddress"
ok "deployed to $COUNTER in block $(cast to-dec "$(echo "$RECEIPT" | jq -r .blockNumber)"), $(cast to-dec "$(echo "$RECEIPT" | jq -r .gasUsed)") gas"

CODE=$(cast code "$COUNTER")
WANT=$(jq -r .deployedBytecode.object out/Counter.sol/Counter.json)
[ "$CODE" = "$WANT" ] && ok "eth_getCode matches the compiled runtime ($(( (${#CODE} - 2) / 2 )) bytes)" \
    || die "eth_getCode returned different code than solc produced"

# --- read, estimate, write -----------------------------------------------------

step "Read with eth_call"
expect "$(cast call "$COUNTER" 'number()(uint256)')" "0" "number()"

step "Estimate, then send increment()"
GAS=$(cast estimate --from "$ME" "$COUNTER" 'increment()')
ok "eth_estimateGas: $GAS"
TX=$(cast send --legacy --private-key "$PK" --gas-limit "$GAS" "$COUNTER" 'increment()' --json)
expect "$(echo "$TX" | jq -r .status)" "0x1" "increment() sent with exactly the estimate"
expect "$(echo "$TX" | jq -r '.logs | length')" "1" "logs emitted"
INC_BLOCK=$(cast to-dec "$(echo "$TX" | jq -r .blockNumber)")

step "State after the transaction"
expect "$(cast call "$COUNTER" 'number()(uint256)')" "1" "number()"
expect "$(cast storage "$COUNTER" 0)" "0x$(printf '%064x' 1)" "eth_getStorageAt slot 0"

step "Find the event with eth_getLogs"
LOGS=$(cast logs --from-block "$INC_BLOCK" --to-block "$INC_BLOCK" --address "$COUNTER" \
    'Incremented(address indexed,uint256)' --json)
expect "$(echo "$LOGS" | jq length)" "1" "Incremented events in block $INC_BLOCK"
expect "$(echo "$LOGS" | jq -r '.[0].topics[0]')" "$(cast keccak 'Incremented(address,uint256)')" "topic 0 (event signature)"
ME_TOPIC=$(cast abi-encode 'f(address)' "$ME")   # an address, left-padded to 32 bytes
expect "$(echo "$LOGS" | jq -r '.[0].topics[1]')" "$ME_TOPIC" "topic 1 (indexed sender)"
expect "$(cast to-dec "$(echo "$LOGS" | jq -r '.[0].data')")" "1" "data (newValue)"
# Filtering by the indexed sender topic, with a null wildcard for the signature.
BY_ME=$(cast rpc eth_getLogs "{\"fromBlock\":\"earliest\",\"address\":\"$COUNTER\",\"topics\":[null,\"$ME_TOPIC\"]}")
expect "$(echo "$BY_ME" | jq length)" "1" "events filtered by indexed sender"

step "eth_call runs the code but keeps nothing"
cast call "$COUNTER" 'increment()' >/dev/null
expect "$(cast call "$COUNTER" 'number()(uint256)')" "1" "number() after a simulated increment()"

step "setNumber(42)"
TX=$(cast send --legacy --private-key "$PK" "$COUNTER" 'setNumber(uint256)' 42 --json)
expect "$(echo "$TX" | jq -r .status)" "0x1" "setNumber status"
expect "$(cast call "$COUNTER" 'number()(uint256)')" "42" "number()"

# --- failure -------------------------------------------------------------------

step "A revert, simulated: eth_call reports the reason"
REASON=$(cast call "$COUNTER" 'fail()' 2>&1 || true)
echo "$REASON" | grep -q 'Counter: nope' && ok "revert reason decoded: Counter: nope" \
    || die "expected the revert reason, got: $REASON"

step "A revert, mined: included, charged, nonce advanced"
NONCE_BEFORE=$(cast nonce "$ME")
# --gas-limit skips estimation, which would (rightly) refuse a call that always
# reverts; we want it mined anyway to show what a failed transaction looks like.
TX=$(cast send --legacy --private-key "$PK" --gas-limit 100000 "$COUNTER" 'fail()' --json 2>/dev/null || true)
expect "$(echo "$TX" | jq -r .status)" "0x0" "receipt status"
ok "gas still paid: $(cast to-dec "$(echo "$TX" | jq -r .gasUsed)")"
expect "$(cast nonce "$ME")" "$((NONCE_BEFORE + 1))" "sender nonce"
BLOCK=$(cast block "$(echo "$TX" | jq -r .blockNumber)" --json)
echo "$BLOCK" | jq -e --arg h "$(echo "$TX" | jq -r .transactionHash)" '.transactions | index($h)' >/dev/null \
    && ok "listed in block $(cast to-dec "$(echo "$BLOCK" | jq -r .number)")" || die "not in its block"
expect "$(cast call "$COUNTER" 'number()(uint256)')" "42" "number() untouched by the revert"

printf '\n\033[1;32mAll checks passed.\033[0m Counter is live at %s\n' "$COUNTER"
