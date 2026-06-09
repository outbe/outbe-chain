# Deployment Guide

This guide describes how to deploy intent contracts across multiple chains and configure cross-chain communication.

> **Prerequisites:** Before deployment, make sure you've completed the installation and build steps described in [README.md](./README.md).

## Prerequisites

1. **LayerZero V2 Endpoint** must be deployed on each chain
2. **The Compact** must be deployed on each chain
3. **CreateX** must be deployed on each chain for deterministic deployment

## Step 1: Prepare Configuration

Edit `.env` file for each chain:

### BSC Testnet (.env)
```bash
NETWORK=bsc-testnet
DEPLOYER_PK=0x...
CONTRACT_SALT=layerzero-v1
CREATEX_ADDRESS=0xba5Ed099633D3B313e4D5F7bdc1305d3c28ba5Ed
ROUTER_OWNER=0xYourOwnerAddress
LZ_ENDPOINT=0x6EDCE65403992e310A62460808c4b910D972f10f
COMPACT_ADDRESS=0x00000000000000171ede64904551eeDF3C6C9788
```

### Outbe-Dev (.env)
```bash
NETWORK=outbe-dev
DEPLOYER_PK=0x...
CONTRACT_SALT=layerzero-v1
CREATEX_ADDRESS=0xba5Ed099633D3B313e4D5F7bdc1305d3c28ba5Ed
ROUTER_OWNER=0xYourOwnerAddress
LZ_ENDPOINT=0x6EDCE65403992e310A62460808c4b910D972f10f
COMPACT_ADDRESS=0x00000000000000171ede64904551eeDF3C6C9788
```

## Step 2: Deploy Router on Each Chain

```bash
# Deploy on Outbe-dev
npm run deployLzRouter
# Save address: OUTBE_ROUTER=0x...

# Deploy on BSC Testnet
npm run deployLzRouter
# Save address: BSC_ROUTER=0x...
```

## Step 3: Configure Router (RouterAllocator + Compact + Peers)

After deploying on all chains, run `ConfigRouter` on each chain.
This script deploys `RouterAllocator`, wires The Compact, and authorizes the router.

Add to `.env`:
```bash
ROUTER_ADDRESS=0xYourDeployedRouterAddress

# Peers (other chains)
PEER_EIDS=40512,40102,40612
PEER_ADDRESSES=0xOutbeRouterAddress,0xBSCRouterAddress,0xOtherRouterAddress
PEER_DOMAINS=512512,97,424242

# Optional: wire solver collateral checks (set after Step 4)
# SOLVER_ESCROW_ADDRESS=0x...
```

Execute:
```bash
npm run configureRouter
```

This will:
1. Deploy `RouterAllocator` → registers with The Compact, gets `ALLOCATOR_ID`
2. Build `LOCK_TAG` via `allocator.buildLockTag(Scope.Multichain, ResetPeriod.OneDay)`
3. Call `router.setCompactConfig(compact, lockTag)`
4. Call `allocator.addOperator(router)` — authorizes the router
5. Configure LayerZero peers via `setPeerWithDomain()`
6. If `SOLVER_ESCROW_ADDRESS` is set — call `router.setSolverEscrow()` to enable collateral checks in auction

## Step 4: Deploy Solver Collateral (Destination Chain Only)

Solver collateral is only needed on the **destination chain** where the auction runs.

```bash
npm run deployEscrow
```

This will:
1. Deploy `SolverAllocator` → registers with The Compact
2. Build `LOCK_TAG` via `allocator.buildLockTag(Scope.SingleChain, ResetPeriod.TenMinutes)`
3. Deploy `SolverEscrow` with the lock tag
4. Call `allocator.setArbiter(escrow)` — authorizes escrow for withdrawals and slashing

Save the escrow address, then re-run Step 3 with `SOLVER_ESCROW_ADDRESS` to wire it into the auction.

## Step 5: Verify Setup

```bash
# Check router config
cast call $ROUTER_ADDRESS "COMPACT()(address)"
cast call $ROUTER_ADDRESS "LOCK_TAG()(bytes12)"

# Check router allocator
cast call $ALLOCATOR_ADDRESS "ALLOCATOR_ID()(uint96)"
cast call $ALLOCATOR_ADDRESS "authorizedOperators(address)(bool)" $ROUTER_ADDRESS

# Check solver escrow
cast call $ESCROW_ADDRESS "LOCK_TAG()(bytes12)"
cast call $SOLVER_ALLOCATOR_ADDRESS "arbiter()(address)"
```
