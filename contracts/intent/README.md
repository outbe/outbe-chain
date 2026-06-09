# ERC7683 Intent Contracts

## Implementation Details

This project implements the [ERC7683 specification](https://github.com/across-protocol/ERCs/blob/master/ERCS/erc-7683.md) with an **auction-based solver selection mechanism**. Solvers compete by submitting quotes during a quoting period, with the best quote winning the right to fill the order.

Token custody on the origin chain is managed by **[The Compact](https://github.com/Uniswap/the-compact)** — an ownerless ERC-6909 resource lock.

This repository provides:
- ERC7683-compliant smart contracts with settlement and refund extensions
- Auction mechanism for competitive solver selection
- Solver collateral system with lock/unlock/slash lifecycle
- LayerZero V2 integration for cross-chain messaging
- TypeScript examples for testing deployed contracts

## Architecture

Contract inheritance structure:

```
                    ┌──────────────────────┐
                    │  ERC7683 Interfaces  │
                    │  IOriginSettler      │
                    │  IDestinationSettler │
                    └──────────────────────┘
                              │
                ┌─────────────┴─────────────┐
                │                           │
                ↓                           ↓
     ┌────────────────────┐      ┌────────────────────┐
     │ OriginSettlerBase  │      │DestinationSettler  │
     │ • open()           │      │ Base               │
     │ • resolve()        │      │ • fill()           │
     │ • allocatedTransfer│      │ • settle()         │
     │                    │      │ • refund()         │
     └─────────┬──────────┘      └──────┬─────────────┘
               │                        │
     ┌─────────┴──────────┐      ┌──────┴───────-───┐    ┌───────────────┐
     │ OriginSettler      │      │DestinationSettler│    │   Auction     │
     │ • settle/refund    │      │ • submitQuote    │───>│               │
     │   via Compact      │      │ • claimOrder     │    │  • quotes     │
     └─────────┬──────────┘      │ • collateral     │    │  • winner     │
               │                 │   hooks          │    └───────────────┘
               │                 └──────┬───────────┘
               └──────────┬─────────────┘
                          │
                    ┌─────┴──────┐
                    │ BaseRouter │
                    │ • Compact  │
                    │ • Auction  │
                    │ • Escrow   │
                    └─────┬──────┘
                          │
              ┌───────────┴───────────┐
              │    LayerZeroRouter    │
              │                       │
              └───────────────────────┘
```

**Components:**
- **OriginSettlerBase/OriginSettler**: Order creation, Compact deposits on open, `allocatedTransfer` on settle/refund
- **DestinationSettlerBase/DestinationSettler**: Order claiming, filling, settlement, and refunds on destination chain. `submitQuote()` with collateral check delegates to Auction.
- **Auction**: contract for competitive solver selection.
- **BaseRouter**: Shared base — Compact config, same-chain dispatch routing, auction/escrow wiring
- **LayerZeroRouter**: Complete deployable implementation — LayerZero V2 messaging adapter
- **SolverEscrow**: Solver collateral via The Compact — deposit, withdraw, lock, unlock, slash. `AUTHORIZED_CALLER` (Router) manages locks.
- **SolverAllocator**: The Compact allocator for solver collateral. Blocks all direct ERC6909 transfers (`attest` reverts). Only the arbiter (escrow) can authorize claims/withdrawals.

### Order Lifecycle

```
╔══════════════════════════════════════════════════════════════════════════════╗
║                    AUCTION-BASED ORDER FULFILLMENT                           ║
╚══════════════════════════════════════════════════════════════════════════════╝

   ORIGIN CHAIN                                    DESTINATION CHAIN
   ════════════                                    ═════════════════

   [1] User Opens Order
       • Deposits inputToken into
         The Compact (ERC6909)
       • Emits Open event
       • Sets fillDeadline
              │
              │ ─────> Solvers watch Open event (off-chain)
              │                     │
              │                     │          [2] Quoting Period
              │                     │              • Solver A: 995 USDT
              │                     │              • Solver B: 998 USDT ✓ Winner
              │                     │              • Solver C: 997 USDT
              │                     │              (requires min collateral in escrow)
              │                     │
              │                     └─────────> [3] Claim Order
              │                                      • Anyone calls claimOrder()
              │                                      • Locks 10% of winner's collateral
              │                                      • Status: UNKNOWN → CLAIMED
              │                                              │
              │                                              ↓
              │                                      [4] Winner Fills Order
              │                                           • Provides outputToken to user
              │                                           • Unlocks collateral
              │                                           • Sends settlement msg
              │                                           • Status: CLAIMED → FILLED
              │                                                  │
              │ <──────────────── LayerZero Message ────────────┘
              │
   [5] Settlement Validated
       • allocatedTransfer: inputToken → winning solver
       • If slashed reward pool has funds: +1.5% bonus to solver


╔══════════════════════════════════════════════════════════════════════════════╗
║                       REFUND PATH: ORDER EXPIRED                            ║
╚══════════════════════════════════════════════════════════════════════════════╝

   ORIGIN CHAIN                                    DESTINATION CHAIN
   ════════════                                    ═════════════════

   [1] Order expired, not filled                     [2] Anyone calls refund()
              │                                         • After fillDeadline
              │                                         • If CLAIMED → slash collateral
              │                                         • If UNKNOWN → no slash
              │                                         • Sends refund message
              │                                                │
              │ <──────────────── LayerZero Message ───────────┘
              │
   [3] Refund Processed
       • allocatedTransfer: inputToken → user


╔══════════════════════════════════════════════════════════════════════════════╗
║                          SAME-CHAIN SWAPS                                    ║
╚══════════════════════════════════════════════════════════════════════════════╝

   When originDomain == destinationDomain, the router acts as both origin and
   destination settler on the same chain. The flow is identical (open → quote →
   claim → fill → settle/refund), but settle() and refund() call the origin
   handlers directly — no cross-chain messaging is involved.

   • No LayerZero fee required (msg.value = 0)
   • Settlement and refund are executed atomically in the same transaction
```

## OriginSettlerBase

Located in [`src/router/origin/OriginSettlerBase.sol`](./src/router/origin/OriginSettlerBase.sol). Entry point on source chain.

**Key functions:**
- `open()` - Create order and deposit inputToken into The Compact
- `resolve()` - Get order details
- `invalidateNonces()` - Replay protection

## DestinationSettlerBase

Located in [`src/router/destination/DestinationSettlerBase.sol`](./src/router/destination/DestinationSettlerBase.sol). Handles order lifecycle on destination.

**Order statuses:** `UNKNOWN` → `CLAIMED` → `FILLED`

**Key functions:**
- `fill()` - Fill order (requires `CLAIMED` status, must be auction winner)
- `settle()` - Trigger settlement to origin (requires `FILLED` status)
- `refund()` - Initiate refund for expired orders (accepts `UNKNOWN` or `CLAIMED`)

**Collateral hooks** (virtual, implemented by DestinationSettler):
- `_onClaimed()` - Lock collateral when order is claimed
- `_onFilled()` - Unlock collateral on successful fill
- `_onSlashed()` - Slash collateral when claimed order expires without fill

## DestinationSettler

Located in [`src/router/destination/DestinationSettler.sol`](./src/router/destination/DestinationSettler.sol). Extends base with auction delegation and collateral logic.

**Key functions:**
- `submitQuote(orderId, outputAmount, orderData)` - Submit quote with order validation and collateral check, delegates to Auction
- `claimOrder(orderId, originData)` - Claim order after quoting ends, locks winner's collateral (10% of output amount)
- `_fillOrder()` - Transfer outputToken to recipient (ERC20 or native)

## Auction

Located in [`src/Auction.sol`](./src/Auction.sol). Competitive solver selection contract (composition).

**How it works:**
1. Quoting period begins on first quote submission
2. Solvers submit quotes via `DestinationSettler.submitQuote()` (collateral check + delegation to Auction)
3. After quoting period ends, highest output amount wins
4. Anyone calls `claimOrder()` — locks 10% of winner's collateral
5. Only winner can call `fill()` — collateral unlocked on success
6. If fill deadline passes without fill — `refund()` slashes the locked collateral



**Key functions:**
- `submitQuote(orderId, outputAmount, solver)` - Record quote with gas cost (onlyRouter). At `claimOrder`. The winner reimburses all losers.
- `resetAuction(orderId, winner)` - Restart auction excluding winner (onlyRouter)
- `getWinner()` - Get winning solver and amount
- `getQuotes()` - View all quotes

**Configuration:**
- `quotingPeriod` - Default: 10 seconds (onlyOwner)
- `maxQuotesPerOrder` - Default: 10 quotes max (onlyOwner)
- `router` - Set via `setRouter()` (onlyOwner)

## SolverEscrow

Located in [`src/SolverEscrow.sol`](./src/SolverEscrow.sol). Manages solver collateral via The Compact.

**Deposit flow:** Solver → Escrow → `Compact.depositAndRegisterFor(recipient=solver, arbiter=escrow)` — solver holds ERC6909 tokens directly.

**Withdraw flow:** Solver → Escrow → `Compact.claim(sponsor=solver)` → tokens → Solver (only unlocked balance).

**Lock/Slash flow:** DestinationSettler → `lockCollateral` → `unlockCollateral` or `slashCollateral`

**Key functions:**
- `deposit(token, amount)` - Deposit ERC20 or native as collateral
- `withdraw(token, amount)` - Withdraw available (unlocked) collateral (0 = withdraw all)
- `lockCollateral(orderId, solver, token, amount)` - Lock collateral for claimed order (authorized caller only)
- `unlockCollateral(orderId)` - Unlock on successful fill (authorized caller only)
- `slashCollateral(orderId)` - Slash via `transferFrom` to escrow (authorized caller only)
- `distributeReward(token, orderAmountIn, receiver)` - Pay 1.5% of order amount from slashed pool to solver on settle (authorized caller only). Returns 0 if pool insufficient.
- `hasMinCollateral(solver, token, outputAmount)` - Check if solver has enough available collateral
- `getBalance(solver, token)` - Get total/locked/available balance
- `getBalances(solver, tokens)` - Batch balance query

## Development

```bash
# Install
npm install

# Build
forge build

# Test
forge test                              # All tests

# Format & Lint
forge fmt                               # Format
forge lint                              # Security checks
```

## Deployment

See [DEPLOYMENT.md](./DEPLOYMENT.md) for production instructions.

## Testing Deployed Contracts

TypeScript examples in [`examples/`](./examples):

See [`examples/README.md`](./examples/README.md) for usage.
