#!/usr/bin/env bash
# zerofee-e2e.sh — end-to-end verification of the EIP-7702 ZeroFee
# paymaster on a live localnet.
#
# Usage:
#   scripts/zerofee-e2e.sh [--keep] [--reuse]
#
#   --keep   leave the testnet running after a successful run
#   --reuse  do NOT bootstrap/start; assume a localnet is already
#            running at $OUTBE_TESTNET_DIR with the first validator's
#            key file readable
#
# What it proves:
#   1. Pectra hardfork is active at genesis (eth_getCode at ZEROFEE
#      returns the EIP-161 marker `0xef`).
#   2. An EIP-7702 self-delegation lands the designator
#      `0xef0100 ++ ZEROFEE_ADDRESS` on the EOA.
#   3. 8 sponsored `claimReward(0)` transactions land with status=1,
#      zero balance delta, a `SponsorshipAuthorized(signer,day,count)`
#      log, and incrementing counter values.
#   4. The 9th sponsored attempt lands IN THE BLOCK with status=0 and
#      an `OutbeFailure(110, ...)` log — proving that the pool
#      admitted the over-quota tx and the executor produced the
#      soft-failure receipt (F2 contract).
#   5. The SAME delegated, quota-exhausted signer can still transact by
#      paying: a tx with `priority_fee > 0` lands with status=1, debits
#      a non-zero fee, leaves the counter at 8, and emits NO
#      `SponsorshipAuthorized` log — proving EIP-7702 delegation is
#      ADDITIVE, not a free-only jail.
#
# Requires `cast` (Foundry) and `python3` on PATH.

set -euo pipefail

KEEP=0
REUSE=0
for arg in "$@"; do
    case "$arg" in
        --keep)  KEEP=1 ;;
        --reuse) REUSE=1 ;;
        *) echo "unknown flag: $arg" >&2; exit 1 ;;
    esac
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUTBE_TESTNET_DIR="${OUTBE_TESTNET_DIR:-/tmp/outbe-zerofee-e2e}"
OUTBE_CHAIN_BINARY="${OUTBE_CHAIN_BINARY:-$REPO_ROOT/target/release/outbe-chain}"
OUTBE_CLI_BINARY="${OUTBE_CLI_BINARY:-$REPO_ROOT/target/release/outbe-cli}"
RPC="${RPC:-http://localhost:8545}"

ZEROFEE_ADDRESS="0x000000000000000000000000000000000000ee09"
AGENT_REWARD_ADDRESS="0x000000000000000000000000000000000000100b"
ZEROFEE_LOG_ADDRESS="0x000000000000000000000000000000000000ee06"

# Pre-computed selectors (keccak256 of the function signatures, first 4 bytes).
CLAIM_REWARD_SELECTOR="0xae169a50"

# Pre-computed event topic0:
#   keccak256("SponsorshipAuthorized(address,uint32,uint32)")
SPONSORSHIP_AUTHORIZED_TOPIC0="0x82fb9fccc7b9033227aa1f5b18f6140ac5a8216361e4e7496146c804bd6e8cc8"

# Verify required tooling.
for cmd in cast python3 curl jq; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "error: required command '$cmd' is not on PATH" >&2
        exit 1
    fi
done
if [ ! -x "$OUTBE_CHAIN_BINARY" ]; then
    echo "error: outbe-chain binary not found at $OUTBE_CHAIN_BINARY" >&2
    exit 1
fi
if [ ! -x "$OUTBE_CLI_BINARY" ]; then
    echo "error: outbe-cli binary not found at $OUTBE_CLI_BINARY" >&2
    exit 1
fi

log()  { printf '\033[36m[e2e]\033[0m %s\n' "$*"; }
pass() { printf '\033[32m[ ok]\033[0m %s\n' "$*"; }
fail() { printf '\033[31m[FAIL]\033[0m %s\n' "$*" >&2; exit 1; }

stop_testnet() {
    if [ "$KEEP" = "1" ]; then
        log "leaving testnet running at $OUTBE_TESTNET_DIR"
        return
    fi
    log "stopping testnet"
    "$REPO_ROOT/scripts/run-testnet.sh" stop "$OUTBE_TESTNET_DIR" >/dev/null 2>&1 || true
}
trap stop_testnet EXIT

# ------------------------------------------------------------------
# Step 0: bootstrap + start (unless --reuse)
# ------------------------------------------------------------------
if [ "$REUSE" = "0" ]; then
    log "bootstrapping clean 4-validator testnet at $OUTBE_TESTNET_DIR"
    rm -rf "$OUTBE_TESTNET_DIR"
    OUTBE_CHAIN_BINARY="$OUTBE_CHAIN_BINARY" \
        "$REPO_ROOT/scripts/bootstrap-testnet.sh" 4 "$OUTBE_TESTNET_DIR" >/dev/null

    if ! grep -q '"pragueTime": 0' "$OUTBE_TESTNET_DIR/genesis.json"; then
        fail "Pectra hardfork not activated at genesis"
    fi
    pass "Pectra hardfork activated at genesis (pragueTime: 0)"

    log "starting 4 validators"
    OUTBE_CHAIN_BINARY="$OUTBE_CHAIN_BINARY" \
        "$REPO_ROOT/scripts/run-testnet.sh" start "$OUTBE_TESTNET_DIR" >/dev/null
fi

# ------------------------------------------------------------------
# Step 1: wait for block production
# ------------------------------------------------------------------
log "waiting for block height >= 1"
until BLOCK_HEX=$(cast block-number --rpc-url "$RPC" 2>/dev/null) && [ "$BLOCK_HEX" -gt 0 ] 2>/dev/null; do
    sleep 2
done
pass "block production started (height=$BLOCK_HEX)"

# ------------------------------------------------------------------
# Step 2: load signer
# ------------------------------------------------------------------
FUNDER_PK_FILE="$OUTBE_TESTNET_DIR/validator-0/evm-key.hex"
[ -r "$FUNDER_PK_FILE" ] || fail "validator-0 key not readable at $FUNDER_PK_FILE"
FUNDER_PK="$(tr -d '[:space:]' < "$FUNDER_PK_FILE")"
[[ "$FUNDER_PK" == 0x* ]] || FUNDER_PK="0x$FUNDER_PK"
FUNDER="$(cast wallet address --private-key "$FUNDER_PK")"

# Use a fresh non-validator EOA as the test signer. Using a validator
# address would let `delta` go negative (the signer also earns chain
# rewards as proposer), masking whether fees were debited. A fresh
# EOA has no income stream so any non-zero delta is a real fee debit.
PK="0x$(python3 -c "import secrets; print(secrets.token_hex(32))")"
SIGNER="$(cast wallet address --private-key "$PK")"
log "test signer (fresh non-validator EOA) = $SIGNER"

# Fund the fresh signer with 1 COEN (10^18 wei) — enough to cover
# the Pectra set-code tx at whatever base fee the localnet quotes.
# The funding amount is also the marker we measure deltas against:
# the sponsored-tx invariant is "balance unchanged after sponsorship",
# which we assert numerically below.
log "funding $SIGNER with 1 COEN from validator-0"
cast send "$SIGNER" --value "1000000000000000000" --private-key "$FUNDER_PK" --rpc-url "$RPC" --json >/dev/null

# ------------------------------------------------------------------
# Step 3: Pectra activation check via marker bytecode
# ------------------------------------------------------------------
ZEROFEE_CODE="$(cast code --rpc-url "$RPC" "$ZEROFEE_ADDRESS")"
[ "$ZEROFEE_CODE" = "0xef" ] || fail "ZEROFEE address has unexpected code: $ZEROFEE_CODE (expected 0xef marker)"
pass "ZEROFEE_ADDRESS holds EIP-161 marker bytecode 0xef"

# ------------------------------------------------------------------
# Step 4a: slot 0 schema version (per README rule "All precompiles
#          storage versioned, slot 0 = version") is seeded by genesis.
# ------------------------------------------------------------------
SCHEMA_HEX="$(cast storage --rpc-url "$RPC" "$ZEROFEE_ADDRESS" 0)"
SCHEMA_DEC="$(python3 -c "print(int('$SCHEMA_HEX', 16))")"
[ "$SCHEMA_DEC" = "1" ] || fail "ZEROFEE slot 0 = $SCHEMA_DEC (expected 1 — schema version)"
pass "ZEROFEE slot 0 = 1 (schema version seeded at genesis)"

# ------------------------------------------------------------------
# Step 4b: ZeroFee precompile responds to both view methods.
# Both are anchored to the current block's UTC day (lazy reset applied).
# ------------------------------------------------------------------
# authorizeSponsorship(address) view returns (bool): a fresh funded
# signer with quota remaining must return true. (The actual EIP-7702
# designator check is the executor's job, not this view — the view is
# the policy predicate only.)
AUTH_VIEW="$(cast call "$ZEROFEE_ADDRESS" \
    "authorizeSponsorship(address)(bool)" "$SIGNER" --rpc-url "$RPC")"
[ "$AUTH_VIEW" = "true" ] || fail "authorizeSponsorship for fresh funded signer = $AUTH_VIEW (expected true)"
pass "ZeroFee dispatches authorizeSponsorship (true for fresh funded signer)"

# getCounter(address) view returns (uint32 day, uint32 count): a fresh
# signer must report count 0 for today (day is today's UTC key, count 0
# because no sponsored tx has burned a slot yet).
COUNTER_TUPLE="$(cast call "$ZEROFEE_ADDRESS" \
    "getCounter(address)(uint32,uint32)" "$SIGNER" --rpc-url "$RPC")"
COUNTER_DAY="$(echo "$COUNTER_TUPLE" | head -1)"
COUNTER_CNT="$(echo "$COUNTER_TUPLE" | tail -1)"
[ "$COUNTER_CNT" = "0" ] \
    || fail "getCounter count for fresh signer = $COUNTER_CNT (expected 0)"
[ "$COUNTER_DAY" != "0" ] \
    || fail "getCounter day for fresh signer = 0 (expected today's UTC day key — lazy reset must anchor to block timestamp)"
pass "ZeroFee dispatches getCounter (fresh signer → today, count 0)"

# ------------------------------------------------------------------
# Step 5: plant EIP-7702 self-delegation
# ------------------------------------------------------------------
# Self-auth nuance (EIP-7702 spec): when the tx sender IS the authority,
# `auth.nonce` must equal `tx.nonce + 1` because the tx increments the
# signer's nonce before the auth list is processed.
CUR_NONCE="$(cast nonce --rpc-url "$RPC" "$SIGNER")"
AUTH_NONCE=$((CUR_NONCE + 1))
log "signing EIP-7702 authorization (auth_nonce=$AUTH_NONCE)"
AUTH="$(cast wallet sign-auth "$ZEROFEE_ADDRESS" \
    --private-key "$PK" \
    --rpc-url "$RPC" \
    --nonce "$AUTH_NONCE")"

# Send the set-code tx. We explicitly set --gas-limit because the tx
# CALLs the signer's own address which, after auth processing, is the
# delegated marker bytecode `0xef` (single byte). revm's gas
# estimation refuses to traverse the invalid-opcode path; that's
# fine — the auth still lands in the alloc step. status=0 on the
# inner call is expected and irrelevant; what matters is the post-tx
# code at the signer.
log "sending Pectra set-code tx (delegate $SIGNER → $ZEROFEE_ADDRESS)"
cast send "$SIGNER" \
    --auth "$AUTH" \
    --private-key "$PK" \
    --rpc-url "$RPC" \
    --gas-limit 100000 \
    --json >/dev/null

DELEG_CODE="$(cast code --rpc-url "$RPC" "$SIGNER" | tr 'A-Z' 'a-z')"
EXPECTED_PREFIX="$(echo "0xef0100${ZEROFEE_ADDRESS:2}" | tr 'A-Z' 'a-z')"
if [ "$DELEG_CODE" != "$EXPECTED_PREFIX" ]; then
    fail "delegation designator not set; got $DELEG_CODE, expected $EXPECTED_PREFIX"
fi
pass "EIP-7702 delegation designator installed on signer (23-byte 0xef0100++ZEROFEE)"

# ------------------------------------------------------------------
# Step 6: 8 sponsored claimReward(0) transactions
# ------------------------------------------------------------------
# claimReward(uint256) selector + 32-byte zero argument.
CALLDATA="${CLAIM_REWARD_SELECTOR}$(printf '%064d' 0)"
BAL_INITIAL="$(cast balance --rpc-url "$RPC" "$SIGNER")"
log "starting 8 sponsored transactions; initial balance = $BAL_INITIAL wei"

for i in 1 2 3 4 5 6 7 8; do
    BAL_BEFORE="$(cast balance --rpc-url "$RPC" "$SIGNER")"
    TX_JSON="$(cast send "$AGENT_REWARD_ADDRESS" \
        --gas-limit 200000 \
        --gas-price 100 \
        --priority-gas-price 0 \
        --private-key "$PK" \
        --rpc-url "$RPC" \
        --json \
        "$CALLDATA")"
    TX_STATUS="$(echo "$TX_JSON" | jq -r '.status')"
    [ "$TX_STATUS" = "0x1" ] || fail "sponsored tx #$i: status=$TX_STATUS (expected 0x1)"

    BAL_AFTER="$(cast balance --rpc-url "$RPC" "$SIGNER")"
    DELTA="$(python3 -c "print(int('$BAL_BEFORE') - int('$BAL_AFTER'))")"
    [ "$DELTA" = "0" ] || fail "sponsored tx #$i: balance delta = $DELTA wei (expected 0 — fee should be waived)"

    HAS_EVENT="$(echo "$TX_JSON" \
        | jq -r --arg addr "$ZEROFEE_ADDRESS" --arg topic "$SPONSORSHIP_AUTHORIZED_TOPIC0" '
            [.logs[] | select(.address == $addr and .topics[0] == $topic)] | length
        ')"
    [ "$HAS_EVENT" = "1" ] || fail "sponsored tx #$i: missing SponsorshipAuthorized log"

    # getCounter applies the lazy reset against the chain's own block
    # timestamp, so the count it reports is robust to host/chain UTC-day
    # skew — read its `count` field (tail of the two-line tuple output).
    COUNTER_VAL="$(cast call "$ZEROFEE_ADDRESS" \
        "getCounter(address)(uint32,uint32)" "$SIGNER" --rpc-url "$RPC" | tail -1)"
    [ "$COUNTER_VAL" = "$i" ] || fail "sponsored tx #$i: counter=$COUNTER_VAL (expected $i)"

    pass "sponsored tx #$i: status=1, fee=0, event=ok, counter=$i"
done

BAL_AFTER_8="$(cast balance --rpc-url "$RPC" "$SIGNER")"
TOTAL_DELTA="$(python3 -c "print(int('$BAL_INITIAL') - int('$BAL_AFTER_8'))")"
[ "$TOTAL_DELTA" = "0" ] || fail "total balance delta across 8 sponsored tx = $TOTAL_DELTA wei (expected 0)"
pass "8 sponsored txs total balance delta = 0 wei (full fee waiver)"

# After 8/8 sponsored use, authorizeSponsorship MUST return false —
# this is the canonical "would the executor admit a 9th?" predicate
# and the view that wallets call before submission.
AUTH_AFTER_8="$(cast call "$ZEROFEE_ADDRESS" \
    "authorizeSponsorship(address)(bool)" "$SIGNER" --rpc-url "$RPC")"
[ "$AUTH_AFTER_8" = "false" ] \
    || fail "authorizeSponsorship after 8/8 = $AUTH_AFTER_8 (expected false)"
pass "authorizeSponsorship flips to false after quota exhausted"

# getCounter MUST report (today, 8).
COUNTER_AFTER_8="$(cast call "$ZEROFEE_ADDRESS" \
    "getCounter(address)(uint32,uint32)" "$SIGNER" --rpc-url "$RPC")"
COUNT_DAY_AFTER="$(echo "$COUNTER_AFTER_8" | head -1)"
COUNT_VAL_AFTER="$(echo "$COUNTER_AFTER_8" | tail -1)"
[ "$COUNT_VAL_AFTER" = "8" ] \
    || fail "getCounter after 8/8 reports count=$COUNT_VAL_AFTER (expected 8)"
pass "getCounter after 8/8 = (day=$COUNT_DAY_AFTER, count=8)"

# ------------------------------------------------------------------
# Step 7: 9th sponsored tx MUST land in block with soft-failure 110
# ------------------------------------------------------------------
log "sending 9th sponsored tx — must land in block with status=0 + OutbeFailure(110)"
TX_JSON_9="$(cast send "$AGENT_REWARD_ADDRESS" \
    --gas-limit 200000 \
    --gas-price 100 \
    --priority-gas-price 0 \
    --private-key "$PK" \
    --rpc-url "$RPC" \
    --json \
    "$CALLDATA" 2>&1 || true)"
TX_STATUS_9="$(echo "$TX_JSON_9" | jq -r '.status' 2>/dev/null || echo "<not-mined>")"

if [ "$TX_STATUS_9" = "<not-mined>" ]; then
    fail "9th tx did not produce a receipt — pool rejected it (F2 contract broken)"
fi
if [ "$TX_STATUS_9" != "0x0" ]; then
    fail "9th tx unexpectedly succeeded (status=$TX_STATUS_9). Counter quota not enforced."
fi
pass "9th tx landed in block with status=0 — pool admitted (F2 OK), executor produced soft-failure"

CODE_TOPIC="$(echo "$TX_JSON_9" \
    | jq -r --arg addr "$ZEROFEE_LOG_ADDRESS" '
        [.logs[] | select(.address == $addr) | .topics[1]] | first // ""
    ')"
if [ -z "$CODE_TOPIC" ]; then
    fail "9th tx receipt has no OutbeFailure log at $ZEROFEE_LOG_ADDRESS"
fi
# Topic[1] is the indexed `code: uint16` — right-most 4 hex chars.
CODE_HEX="${CODE_TOPIC: -4}"
CODE_DEC="$((16#$CODE_HEX))"
if [ "$CODE_DEC" != "110" ]; then
    fail "9th tx OutbeFailure code = $CODE_DEC (expected 110 FreeTxDailyExhausted)"
fi
pass "9th tx OutbeFailure code = 110 (FreeTxDailyExhausted)"

BAL_AFTER_9="$(cast balance --rpc-url "$RPC" "$SIGNER")"
DELTA_9="$(python3 -c "print(int('$BAL_AFTER_8') - int('$BAL_AFTER_9'))")"
[ "$DELTA_9" = "0" ] || fail "9th rejected tx debited $DELTA_9 wei (expected 0)"
pass "9th rejected tx balance delta = 0 wei"

# ------------------------------------------------------------------
# Step 7b: additive delegation — the quota-exhausted, still-delegated
#          signer pays normally with a tip (priority_fee > 0).
# ------------------------------------------------------------------
# The signer is delegated to the paymaster AND has used all 8 free
# slots. A tx that sets a non-zero priority fee does NOT match the free
# envelope (`classify_sponsorship` requires priority_fee == 0), so both
# the pool and the executor route it through the normal fee path. This
# proves delegation never locks an account into free-only mode.
log "sending a PAYING tx (priority_fee>0) from the quota-exhausted delegated signer"
BAL_BEFORE_PAY="$(cast balance --rpc-url "$RPC" "$SIGNER")"
TX_JSON_PAY="$(cast send "$AGENT_REWARD_ADDRESS" \
    --gas-limit 200000 \
    --gas-price 1000000000 \
    --priority-gas-price 1000000000 \
    --private-key "$PK" \
    --rpc-url "$RPC" \
    --json \
    "$CALLDATA" 2>&1 || true)"
TX_STATUS_PAY="$(echo "$TX_JSON_PAY" | jq -r '.status' 2>/dev/null || echo "<not-mined>")"
[ "$TX_STATUS_PAY" = "<not-mined>" ] \
    && fail "paying tx did not produce a receipt — delegated account wrongly blocked from paying"
[ "$TX_STATUS_PAY" = "0x1" ] \
    || fail "paying tx status=$TX_STATUS_PAY (expected 0x1 — delegated account must be able to pay)"

BAL_AFTER_PAY="$(cast balance --rpc-url "$RPC" "$SIGNER")"
DELTA_PAY="$(python3 -c "print(int('$BAL_BEFORE_PAY') - int('$BAL_AFTER_PAY'))")"
[ "$(python3 -c "print(1 if $DELTA_PAY > 0 else 0)")" = "1" ] \
    || fail "paying tx debited $DELTA_PAY wei (expected > 0 — fee must be charged on the normal path)"
pass "paying tx landed with status=1 and debited $DELTA_PAY wei (additive delegation OK)"

# The paying tx must NOT burn a free slot: counter stays at 8.
COUNTER_AFTER_PAY="$(cast call "$ZEROFEE_ADDRESS" \
    "getCounter(address)(uint32,uint32)" "$SIGNER" --rpc-url "$RPC" | tail -1)"
[ "$COUNTER_AFTER_PAY" = "8" ] \
    || fail "paying tx changed counter to $COUNTER_AFTER_PAY (expected 8 — paid txs must not touch the quota)"
pass "paying tx left counter at 8 (no free slot consumed)"

# The paying tx must NOT emit a SponsorshipAuthorized log.
SPONSOR_LOGS_PAY="$(echo "$TX_JSON_PAY" \
    | jq -r --arg addr "$ZEROFEE_ADDRESS" --arg topic "$SPONSORSHIP_AUTHORIZED_TOPIC0" '
        [.logs[] | select(.address == $addr and .topics[0] == $topic)] | length
    ' 2>/dev/null || echo "0")"
[ "$SPONSOR_LOGS_PAY" = "0" ] \
    || fail "paying tx emitted $SPONSOR_LOGS_PAY SponsorshipAuthorized log(s) (expected 0 — it was not sponsored)"
pass "paying tx emitted no SponsorshipAuthorized log (not sponsored)"

# ------------------------------------------------------------------
# Step 8: outbe-cli zero-fee eip7702-authorize signs an Authorization
#         that decodes to the canonical RLP shape
# ------------------------------------------------------------------
CLI_AUTH_JSON="$("$OUTBE_CLI_BINARY" --rpc-url "$RPC" --private-key "$PK" \
    zero-fee eip7702-authorize 2>/dev/null)"
CLI_ADDR="$(echo "$CLI_AUTH_JSON" | jq -r .address | tr 'A-Z' 'a-z')"
CLI_CHAIN="$(echo "$CLI_AUTH_JSON" | jq -r .chainId)"
[ "$CLI_ADDR" = "$ZEROFEE_ADDRESS" ] \
    || fail "outbe-cli authorization address = $CLI_ADDR (expected $ZEROFEE_ADDRESS)"
# `cast chain-id` prints decimal; CLI emits hex. Normalise both to
# decimal before comparison.
RPC_CHAIN_DEC="$(cast chain-id --rpc-url "$RPC")"
CLI_CHAIN_DEC="$(python3 -c "print(int('$CLI_CHAIN', 16))")"
[ "$CLI_CHAIN_DEC" = "$RPC_CHAIN_DEC" ] \
    || fail "outbe-cli chainId = $CLI_CHAIN_DEC (expected RPC value $RPC_CHAIN_DEC)"
pass "outbe-cli zero-fee eip7702-authorize emits canonical SignedAuthorization JSON"

echo
echo "================================================================"
pass "ALL E2E ASSERTIONS PASSED"
echo "================================================================"
