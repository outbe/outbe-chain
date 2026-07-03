# Credis User-Flow Demo (`examples/credis-flow`)

End-to-end TypeScript scripts that drive the Credis system on the Outbe chain. Each
file under `src/` is a standalone runnable that exercises one step of the user / CCA
flow — from pledging Gratis to repaying anadosis and unpledging.

### ⚠ ABI drift vs current outbe-chain

The scripts were authored against a pre-shielded version of Gratis/Credis where
the factory exposed direct `getPledgeTicketByAddress`, `getAllPledgeTickets`, and
a simple-args `unpledgeGratis`/`requestCredis`/`anadosis(positionId, amount)`.
Current outbe-chain moved the pool surface to `IGratisPool` and switched to a
shielded design (merkle commitments, nullifier hashes, ZK proofs in
`unpledgeGratis(args)` and `requestCredis(asset, vaultProvider, bundleAccount, args)`).
`anadosis` now takes only `positionId`. `npm run generate-types` succeeds and
produces working bindings for all 10 staged ABIs (including `IGratisPool`),
but `npx tsc --noEmit` against the imported scripts surfaces ~20 call-site
mismatches — they need to be ported to the new shielded interfaces before the
flow runs end-to-end. The imported scripts are kept verbatim as the porting
reference.

Contract bindings come from this repo's own ABIs. `npm run generate-types` first
runs `scripts/prepare-abis.mjs`, which copies the required JSONs out of
`../../contracts/precompiles/abi-export/`
and `../../contracts/account-abstraction/{abi-export,out}/` into a local
`abi/` directory, then typechain generates ethers v6 factories into
`src/contracts/`. Both directories are gitignored and regenerated on every build.

### First-run quickstart

```bash
cd examples/credis-flow
npm install
npm run generate-types
cp .local-reth.env.example .local-reth.env   # then fill values
npm run info                                  # read-only state snapshot
```

The `.local-reth.env` file is resolved relative to the project root by
`loadEnv(import.meta.url, "local-reth")` in `src/utils.ts`. Override the
environment name by editing `DEFAULT_ENV` at the top of `utils.ts`.

---


## Layout

```
src/
├── 0-info.ts                   Print current state of all actors
├── 0-setup-native.ts           Fund user + CCA with native COEN
├── 0-setup-erc20.ts            Mint / move ERC20 into user + vault provider
├── 1-pledge-gratis.ts          User pledges Gratis with a commitment
├── 1.1-unpledge-gratis.ts      (Sanity) unpledge directly without going through credis
├── 2-top-up-smart-account.ts   Deploy bundle account; transfer ERC20 into it
├── 3-request-credis.ts         CCA calls requestCredis; vault funds enter bundle balance
├── 4-cca-simulate-purchase.ts  CCA uses bundle funds via per-token permission
├── 4.1-user-sa-withdraw.ts     User withdraws their free (non-bundled) balance
├── 5-user-pays-anadosis.ts     User repays an installment via batched UserOp
└── 6-user-unpledge-gratis.ts   User unpledges Gratis using the original secret
```

## Installation

```bash
cd examples/credis-flow
npm install
# Stage local abi/ from outbe-chain/contracts and run typechain
npm run generate-types
```

`npm run generate-types` reads JSON ABI files produced by `make export-abi` in the
parent project (`abi-export/*.json`) and emits `src/contracts/`.

## Configuration

Each script reads two env files from the project root, selected by the `envName`
CLI argument (default: `local-reth`):

- `.${envName}.env` — RPC URL, private keys, fixed addresses
- `.${envName}.deployment.env` — addresses produced by the Foundry deploy scripts

### Outbe Testnet Peira env

Add this to `.outbe-peira.env`:

```dotenv

export RPC_URL="https://peira-rpc.outbe.net"

# modules
export GRATIS_ADDRESS=0x0000000000000000000000000000000000001003
export GRATIS_FACTORY_ADDRESS=0x0000000000000000000000000000000000002003
export CREDIS_ADDRESS=0x000000000000000000000000000000000000100A
export CREDIS_FACTORY_ADDRESS=0x0000000000000000000000000000000000001009

# addresses and keys
export PRIVATE_KEY=8365107f4bd3e538431e7c8dcdd806b2eedba7ae095b846dc8eca0db18bb9b91
export OWNER_ADDRESS=0xDBf385DF0931F78B792A9D040758fc47Ea838386

export USER_PRIVATE_KEY=0xef902d357ec36a786a0c091442a6fc3ae7176e71f33203c533168549f8311b78
export USER_ADDRESS=0x64CCA861d30714593cB690e0a550C8a9b8b3b058

export ERC20_HOLDER_PRIVATE_KEY=0x4d9607c0fcf9d2aa80fb7600cbb2f4aa5d36281145f1103509cb62d3a48836b5

export CCA_PRIVATE_KEY=0x4d1e6508b6901e2dec9e65aeda66cfd4137013056d50c45742daa13fc73f928a
export CCA_ADDRESS=0xbb94B1816c439d84B1C0b43E56b05EE7f2eA0e35

export ERC20_ADDRESS=0x99142E5359d0492783751964eA1a500686538E8C
```

Add this to `.outbe-peira.deployment.env`:

```dotenv


# Kernel stack deployment at block 240467 timestamp 1779359920
export ENTRYPOINT_ADDRESS=0x0000000071727De22E5E9d8BAf0edAc6f37da032
export KERNEL_ADDRESS=0x51Af4C11f3b825E78F672065D80e2056E05bB305
export KERNEL_FACTORY_ADDRESS=0x798749411f57927230fFa2Cce094B451274E04D6
export ECDSA_VALIDATOR_ADDRESS=0x17B1B20Eb874d03f3221Cc4E40295cD5a7362c6B
export CALLER_HOOK_ADDRESS=0xE8C165907Ee014ebdD8eFFF70dad66f99165e9E2
export ECDSA_SIGNER_ADDRESS=0xCB52935BB59c23212fa9fBCAa9C55783Da6586Fc
# Smart account stack deployment at block 240503 timestamp 1779359929
export BUNDLE_MODULE_PLUGIN_ADDRESS=0xfCEf88AdF45644C6eDB7cE44E9d091a47cdD0Bd3
export WITHDRAWAL_LIMIT_POLICY_ADDRESS=0x9020b3C3033d1c1201b8e881C09C96Fe93460492
export BUNDLE_SPEND_PROTECTOR_HOOK_ADDRESS=0xC3Fdf1E3DE6767eeEa95028A8BF93817CA270BDF
export BUNDLE_WITHDRAW_HOOK_ADDRESS=0xdF25D88FED0FF8af2003Eb98E0CC153303fcAF2c
export SMART_ACCOUNT_FACTORY_ADDRESS=0xe28db1d1a138B21f2c84D7156b4Dab45a2F18E30

export VAULT_ADDRESS=0xc0E713890eC7bbcC9e21e027c357c5042B7f03B6
export VAULT_PROVIDER_IMPL_ADDRESS=0x7c43B530dE37E6943f8AfF0e0698246A7b87D682
export VAULT_PROVIDER_ADDRESS=0xA447d123a93236A64CBBE1599E8102b54491F01E
```

## Running

All scripts accept `[envName]` as an optional last positional argument. Each prints
state before / after and a `CHANGES` summary.

```bash
# Show current state
npx tsx src/0-info.ts                                   # default env: local-reth
npx tsx src/0-info.ts outbe-peira

# Setup
npx tsx src/0-setup-native.ts
npx tsx src/0-setup-erc20.ts

# User pledges 77 Gratis with a random commitment
npx tsx src/1-pledge-gratis.ts                          # default amount/commitment
npx tsx src/1-pledge-gratis.ts outbe-peira 77000000000000000000 0xabc...   # amount + commitment

# Deploy bundle account (if needed) and fund with 1,000 USD
npx tsx src/2-top-up-smart-account.ts

# CCA requests credis using a prior pledge commitment
npx tsx src/3-request-credis.ts <commitment>

# CCA spends from the bundle (within the daily limit policy)
npx tsx src/4-cca-simulate-purchase.ts

# Optional: user withdraws their free balance
npx tsx src/4.1-user-sa-withdraw.ts 5.5

# User pays the next anadosis on a credis position
npx tsx src/5-user-pays-anadosis.ts <positionId>

# User unpledges the originally pledged Gratis
npx tsx src/6-user-unpledge-gratis.ts <secret>
```
