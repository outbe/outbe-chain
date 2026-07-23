#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "usage: mise run ocomp-poc-task -- OCM-NN" >&2
  exit 2
fi

task="$1"
case "$task" in
  OCM-00)
    cargo test --locked -p outbe-e2e-harness --test ocomp_evidence_verifier
    cargo run --locked -p outbe-e2e-harness --bin outbe-e2e-evidence -- \
      task-progress OCM-00 --passed OCM-EVD-001
    ;;
  *)
    cargo run --locked -p outbe-e2e-harness --bin outbe-e2e-evidence -- discover
    echo "$task is MISSING: its implementation gate has not been wired yet" >&2
    exit 1
    ;;
esac
