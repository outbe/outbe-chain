#!/usr/bin/env bash
# e2e.md S7 (stale-join half) — a validator with stale state is NOT activated until
# it is caught up. Exercises the stale-join activation guard (confirmValidatorReady).
#
# A staked PENDING joiner that has NOT confirmed readiness must be excluded from the
# DKG reshare target (get_reshare_target_set), so it stays PENDING across a full
# reshare cycle instead of being flipped ACTIVE while behind. Once it confirms
# (operator sends confirm-ready after outbe_syncStatus shows it at tip), the next
# reshare activates it.
E2E_NAME=S7B
source "$(dirname "$0")/lib.sh"

e2e_step "S7b: stake a joiner but do NOT confirm readiness"
e2e_cleanup
e2e_bootstrap 4 || { e2e_summary; exit 1; }
e2e_start
e2e_provision_joiner
e2e_launch_joiner
e2e_wait_height 8549 25 18 >/dev/null
e2e_stake "$V5_KEY" 1000; sleep 6
e2e_assert_eq "joiner is PENDING after stake" "1" "$(e2e_status "$V5_ADDR")"
e2e_log "deliberately NOT sending confirm-ready (simulating a not-yet-synced/stale joiner)"

e2e_step "S7b: it must stay PENDING across a full reshare cycle (guard holds)"
# The first reshare activates at the epoch boundary (~h120 on the dev epoch=120).
# Wait until the committee is well past it (h>130) — this GUARANTEES a reshare ran
# and activated. v5 must remain PENDING the whole way (the guard kept the
# unconfirmed joiner out of the frozen target). Height-based so it does not depend
# on parsing committee logs.
STAYED_PENDING=true; CROSSED=false
for i in $(seq 1 40); do
  sleep 10
  CH=$(e2e_h 8545); ST=$(e2e_status "$V5_ADDR")
  e2e_log "  committee=$CH active=$(e2e_active) v5_status=$ST"
  [ "$ST" != "1" ] && STAYED_PENDING=false
  { [ "$CH" != "dn" ] && [ "$CH" -gt 130 ] 2>/dev/null; } && { CROSSED=true; break; }
done
e2e_assert "committee crossed a reshare/activation (height > 130)" "$([ "$CROSSED" = true ] && echo true || echo false)"
e2e_assert "unconfirmed joiner stayed PENDING across the whole reshare window" "$([ "$STAYED_PENDING" = true ] && echo true || echo false)"
sleep 10
e2e_assert_eq "unconfirmed PENDING joiner was NOT activated (still PENDING)" "1" "$(e2e_status "$V5_ADDR")"
e2e_assert_eq "active set NOT grown by an unconfirmed joiner" "4" "$(e2e_active)"
e2e_assert_eq "unconfirmed joiner is not a participant" "false" "$(e2e_participant "$V5_ADDR")"

e2e_step "S7b: confirm readiness -> next reshare activates it"
e2e_confirm_ready "$V5_KEY"; e2e_log "confirm-ready sent at committee h=$(e2e_h 8545)"
ACT=false
for i in $(seq 1 40); do sleep 10; e2e_log "  committee=$(e2e_h 8545) active=$(e2e_active) v5_status=$(e2e_status "$V5_ADDR")"; [ "$(e2e_participant "$V5_ADDR")" = "true" ] && { ACT=true; break; }; done
e2e_assert "confirmed joiner activates on the next reshare" "$([ "$ACT" = true ] && echo true || echo false)"
e2e_assert_eq "joiner now ACTIVE (2)" "2" "$(e2e_status "$V5_ADDR")"
e2e_assert_eq "active set grew to 5" "5" "$(e2e_active)"

e2e_summary
