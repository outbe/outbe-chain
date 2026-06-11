#!/usr/bin/env bash
# e2e.md S7 (slashing half) — validator misbehavior / liveness under node loss.
#
# Adapted (README.md). Two findings drive this script's shape:
#  1. A felony JAILS the validator (ACTIVE->JAILED) + slashes + bumps slash_count
#     + freezes it (was force-exit ACTIVE->EXITING); from JAILED it can later unjail
#     (->PENDING->ACTIVE) or unstake out.
#  2. DOWNTIME slashing is gated on late-finalize FEE-ESCROW settlement:
#     record_window_close_absentees() only runs when window_close_credited(fb) is
#     Some, which requires rewards.pending_fb_hash_at[fb] != 0 — i.e. an N+K fee
#     escrow to settle (crates/blockchain/evm/src/begin_block_precompile.rs:716-781,
#     crates/system/rewards/src/late_settlement.rs:274). On the ZeroFee localnet
#     with no fee activity that escrow is empty, so a simple node-kill accrues NO
#     voter_miss_count and never trips the felony (verified: 0 misses after ~100
#     blocks with one validator dead). Downtime slashing therefore cannot be
#     triggered by a bare kill on this localnet; the slash MECHANISM (force_exit +
#     slash_stake + felony threshold) and the operator-submitted evidence path
#     (double-proposal / conflicting-vote, real BLS verification) are covered by
#     `cargo nextest -p outbe-slashindicator`.
#
# So the shell-testable assertion here is LIVENESS: killing one validator drops the
# committee to 3-of-4, the chain keeps finalizing (BFT quorum), and the slashing
# configuration surface is present.
E2E_NAME=S7A
source "$(dirname "$0")/lib.sh"

e2e_step "S7a: bootstrap + verify the slashing config surface is present"
e2e_cleanup
e2e_bootstrap 4 || { e2e_summary; exit 1; }
e2e_start
SLASH_PCT=$("$E2E_CLI" slash config --rpc-url $RPC0 2>/dev/null | grep -iE "slash amount" | grep -oE "[0-9]+" | head -1)
e2e_assert "slashing config readable (felony slash percent present)" "$([ -n "$SLASH_PCT" ] && echo true || echo false)"
VICTIM=$(cast wallet address --private-key "$(e2e_vkey 3)")
e2e_assert_eq "victim starts ACTIVE (2)" "2" "$(e2e_status "$VICTIM")"

e2e_step "S7a: kill a validator -> chain stays live on 3-of-4 BFT quorum"
H0=$(e2e_h 8545)
e2e_kill_validator 3
e2e_log "killed validator-3 at h=$H0; verifying the surviving 3 keep finalizing..."
GREW=false; PREV=$H0
for i in $(seq 1 12); do
  sleep 8
  H=$(e2e_h 8545)
  e2e_log "  survivors head=$H (victim_status=$(e2e_status "$VICTIM") voter_miss=$(e2e_slashcount "$VICTIM"))"
  { [ "$H" != "dn" ] && [ "$H" -gt $((PREV)) ] 2>/dev/null; } && GREW=true
  [ "$H" != "dn" ] && [ "$H" -gt $((H0+15)) ] 2>/dev/null && break
  PREV=$H
done
e2e_assert "chain keeps finalizing after losing 1 of 4 validators (BFT quorum)" "$([ "$GREW" = true ] && echo true || echo false)"
e2e_assert "survivors advanced well past the kill height" "$([ "$(e2e_h 8545)" != "dn" ] && [ "$(e2e_h 8545)" -gt $((H0+15)) ] 2>/dev/null && echo true || echo false)"
e2e_log "NOTE: downtime felony is fee-settlement-gated (inactive on ZeroFee localnet); slash + evidence paths are covered by outbe-slashindicator unit tests."

e2e_summary
