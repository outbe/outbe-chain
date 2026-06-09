# LayerZero7683 TypeScript Examples

TypeScript examples for interacting with LayerZero7683 cross-chain swap contracts.

## Setup

```bash
npm install
npm run build
```

Copy `.env.example` to `.env` and set:
- `PRIVATE_KEY` — wallet private key
- `ROUTER` — LayerZero7683 router address
- `INPUT_TOKEN` / `OUTPUT_TOKEN` — token addresses
- RPC URLs and chain IDs per chain

## Scripts

### Orders

```bash
# Open cross-chain order (origin, dest, amountIn, amountOut)
npm run open-order bsc sepolia 10 9.5

# List orders on a chain
npm run list-orders bsc

# Fill order by ID
npm run fill-by-id sepolia 0x1234...

# Settle filled order (sends cross-chain message to pay filler)
npm run settle-by-id bsc 0x1234...

# Refund specific order by ID
npm run refund-by-id bsc 0x1234...

# Refund all expired orders (origin, dest, [blocksBack])
npm run refund-all-latest bsc sepolia
```

### Auction

```bash
# Submit quote as solver
npm run submit-quote outbe_dev 0x1234... 0.0011

# List quotes for order
npm run list-quotes outbe_dev 0x1234...
```

### Solver Escrow

```bash
# Check collateral balance (chain, token|native, [solver])
npm run solver:balance outbe_dev 0x5cDF...Ece

# Deposit collateral (chain, token|native, amount)
npm run solver:deposit outbe_dev 0x5cDF...Ece 100
npm run solver:deposit outbe_dev native 0.1

# Withdraw collateral (chain, nonce, token|native, registeredAmount in wei)
npm run solver:withdraw outbe_dev 0 0x5cDF...Ece 1000000000000000000
```

### Utils

```bash
# Check token balances
npm run check-balance 0x6ecf4efa4f09ae9e7a98e81d41de9bfd7912653f bsc outbe_dev
```

## Available Chains

| Key | Network |
|-----|---------|
| `bsc` | BSC Testnet |
| `sepolia` | Ethereum Sepolia |
| `outbe_priv` | Outbe Privnet |
| `outbe_dev` | Outbe Devnet |

## TypeChain

```bash
npm run typechain   # regenerate types from ../abi/*.json
```
