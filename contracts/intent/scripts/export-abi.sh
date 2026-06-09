#!/usr/bin/env bash
set -euo pipefail

# Contracts to export ABIs for
CONTRACTS=(
  "LayerZeroRouter"
  "ERC20"
  "SolverEscrow"
  "Auction"
)

OUT_DIR="./abi"
mkdir -p "$OUT_DIR"

for name in "${CONTRACTS[@]}"; do
  src=$(find ./out -path "*/${name}.sol/${name}.json" -print -quit)
  if [ -z "$src" ]; then
    echo "⚠ ${name}: not found in ./out, skipping"
    continue
  fi
  jq -r '.abi' "$src" > "${OUT_DIR}/${name}.json"
  echo "✓ ${name}"
done

echo "ABI exported to ${OUT_DIR}/"
