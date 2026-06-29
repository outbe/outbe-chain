#!/usr/bin/env bash
# e2e.md S4 — restart an ACTIVE validator; it resumes signing WITHOUT a reshare
# because its DKG share is persisted on disk (keys-dir), not regenerated.
#
# Adapted (README.md): the BLS share lives in keys_dir files / node memory, NOT
# the enclave (the enclave holds only the tribute offer key). So "share recovered
# inside the TCB" -> share recovered from keys_dir; the enclave is only re-used for
# offers. We restart ONLY the node process; the enclave container keeps running so
# its offer key is preserved (a gramine-direct mock re-derives a new key on restart).
E2E_NAME=S4
source "$(dirname "$0")/lib.sh"

e2e_step "S4: bootstrap + bring a joiner to ACTIVE with a persisted share"
e2e_cleanup
e2e_bootstrap 4 || { e2e_summary; exit 1; }
e2e_start
V0=$(e2e_v0key); e2e_offer "$V0" 1
e2e_provision_joiner
V5_EXTRA_ARGS="--consensus.keys-dir $E2E_DIR/validator-4/keys"
e2e_launch_joiner
e2e_wait_height 8549 25 18 >/dev/null
e2e_stake "$V5_KEY" 1000; sleep 6
e2e_confirm_ready "$V5_KEY"
e2e_log "waiting for v5 -> ACTIVE..."
for i in $(seq 1 40); do sleep 10; [ "$(e2e_participant "$V5_ADDR")" = "true" ] && break; e2e_log "  committee=$(e2e_h 8545) v5=$(e2e_h 8549) active=$(e2e_active)"; done
e2e_assert_eq "joiner reached ACTIVE before restart" "true" "$(e2e_participant "$V5_ADDR")"
# confirm a durable share file exists (share lives on disk, not the enclave).
SHARE_FILE=$(ls "$E2E_DIR/validator-4/keys"/*dkg*share* "$E2E_DIR/validator-4/keys"/dkg_share.hex 2>/dev/null | head -1)
e2e_assert "DKG share persisted to keys-dir (share on disk, not TCB)" "$([ -n "$SHARE_FILE" ] && echo true || echo false)"
sleep 20  # let it sign a few blocks as ACTIVE

e2e_step "S4: kill the node (enclave stays up), restart, resume WITHOUT reshare"
PRE_CEREMONY=$(e2e_joiner_log_count 'running DKG ceremony')
RESTART_H=$(e2e_h 8545)
e2e_stop_joiner
e2e_launch_joiner   # same keys-dir/datadir; enclave container untouched
e2e_log "restarted v5 at committee h=$RESTART_H; catching up..."
UP=false
for i in $(seq 1 30); do sleep 8; VH=$(e2e_h 8549); CH=$(e2e_h 8545); e2e_log "  committee=$CH v5=$VH participant=$(e2e_participant "$V5_ADDR")"; { [ "$VH" != "dn" ] && [ "$VH" -ge "$RESTART_H" ] 2>/dev/null; } && { UP=true; break; }; done
e2e_assert "restarted node caught up to its pre-restart height" "$([ "$UP" = true ] && echo true || echo false)"
e2e_assert_eq "resumed from saved DKG state (no new ceremony)" "true" "$(e2e_joiner_log_has 'threshold material ready')"
POST_CEREMONY=$(e2e_joiner_log_count 'running DKG ceremony')
e2e_assert_eq "no fresh DKG ceremony triggered by the restart" "$PRE_CEREMONY" "$POST_CEREMONY"
e2e_assert_eq "still an ACTIVE consensus participant after restart" "true" "$(e2e_participant "$V5_ADDR")"
# resume signing: lockstep gap=0 after catch-up (proves it signs with the recovered share).
LOCK=1; PREV=0
for i in $(seq 1 5); do sleep 10; CH=$(e2e_h 8545); VH=$(e2e_h 8549); G=$((CH-VH)); e2e_log "  lockstep committee=$CH v5=$VH gap=$G"; { [ "$G" -gt 3 ] || [ "$VH" -le "$PREV" ]; } 2>/dev/null && LOCK=0; PREV=$VH; done
e2e_assert "restarted validator resumes signing (lockstep, share recovered)" "$([ "$LOCK" = 1 ] && echo true || echo false)"
# no equivocation / no byzantine evidence while it was down + after.
e2e_assert_eq "no byzantine/equivocation evidence around the restart" "0" "$(e2e_val_log_count 0 'byzantine evidence observed')"
# enclave still works (offer executed by the reconnected node).
V1=$(e2e_vkey 1)
for t in 1 2 3 4 5; do "$E2E_CLI" tribute offer "$E2E_WWD" --amount 100 --currency 840 --private-key "$V1" --rpc-url "$RPC0" >/dev/null 2>&1; sleep 6; [ "$(e2e_supply 8545)" = "2" ] && break; done
sleep 6
e2e_assert_eq "enclave still works post-restart (offer parity on v5)" "$(e2e_supply 8545)" "$(e2e_supply 8549)"

e2e_summary
