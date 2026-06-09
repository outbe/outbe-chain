#!/usr/bin/env bash
set -euo pipefail

# Deploy Vault and VaultProvider using the configuration for the given environment.
# Usage: ./deploy.sh [env_name]
#   env_name defaults to "local-reth"
# Prerequisites: A matching `.<env_name>.env` file must exist with RPC_URL, PRIVATE_KEY, and ERC20_ADDRESS.
#
# The Solidity scripts append `export VAR=0x...` lines to $DEPLOYMENT_ENV_FILE.
# That path is exported so BaseScript.deploymentFilePath() writes to the same
# file the shell sources from, regardless of which chain id the RPC reports.

ENV_NAME="${1:-local-reth}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$SCRIPT_DIR/.${ENV_NAME}.env"
export DEPLOYMENT_ENV_FILE="$SCRIPT_DIR/.${ENV_NAME}.deployment.env"

if [[ ! -f "$ENV_FILE" ]]; then
  echo "Error: $ENV_FILE not found"
  exit 1
fi

echo "=== Using environment: $ENV_NAME ==="
source "$ENV_FILE"

# Reset the deployment file so a fresh run does not concatenate stale entries
# under the new run's `# ... deployment at block N timestamp T` header.
: > "$DEPLOYMENT_ENV_FILE"

FORGE_COMMON_FLAGS="--rpc-url $RPC_URL --private-key $PRIVATE_KEY --broadcast --gas-estimate-multiplier 300 -vvv"

echo "=== Step 1: Deploy VaultProvider ==="
forge script script/DeployVaultProvider.s.sol $FORGE_COMMON_FLAGS

source "$DEPLOYMENT_ENV_FILE"

echo ""
echo "VaultProvider: ${VAULT_PROVIDER_ADDRESS:-<unset>}"

echo ""
echo "=== Step 2: Set Gates ==="
forge script script/SetVaultGates.s.sol $FORGE_COMMON_FLAGS

echo ""
echo "=== Deployment complete ==="
echo "All addresses written to $DEPLOYMENT_ENV_FILE"
