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
    python3 -m unittest scripts/reference/tests/test_lysis_v1.py
    cargo test --locked -p outbe-lysis --test program_v1_reference
    cargo run --locked -p outbe-e2e-harness --bin outbe-e2e-evidence -- \
      task-progress OCM-00 --passed OCM-EVD-001 --passed OCM-SEM-001
    ;;
  OCM-01)
    cargo test --locked -p outbe-e2e-harness --test ocomp_evidence_verifier
    python3 -m unittest scripts/reference/tests/test_lysis_v1.py
    cargo test --locked -p outbe-lysis
    cargo clippy --locked -p outbe-lysis --all-targets -- -D warnings
    cargo run --locked -p outbe-e2e-harness --bin outbe-e2e-evidence -- \
      task-progress OCM-01 --passed OCM-EVD-001 --passed OCM-SEM-001
    ;;
  OCM-02)
    cargo test --locked -p outbe-e2e-harness --test ocomp_evidence_verifier
    python3 -m unittest scripts/reference/tests/test_lysis_v1.py
    cargo test --locked -p outbe-lysis --test program_v1_reference
    python3 crates/system/ocomp-protocol/registry/generate.py --check
    cargo test --locked -p outbe-ocomp-protocol
    cargo clippy --locked -p outbe-ocomp-protocol --all-targets -- -D warnings
    cargo run --locked -p outbe-e2e-harness --bin outbe-e2e-evidence -- \
      task-progress OCM-02 --passed OCM-EVD-001 --passed OCM-SEM-001
    ;;
  *)
    cargo run --locked -p outbe-e2e-harness --bin outbe-e2e-evidence -- discover
    echo "$task is MISSING: its implementation gate has not been wired yet" >&2
    exit 1
    ;;
esac
