/**
 * 0-setup-erc20.ts
 *
 * Ensures the vault router authorizes the Credis precompiles and that user and
 * vault have sufficient ERC20 balances, using a dedicated holder wallet
 * (ERC20_HOLDER_PRIVATE_KEY) as the source of distributed tokens. The owner
 * (PRIVATE_KEY) mints fresh tokens to the holder only when the holder's balance
 * is insufficient to cover the deficits.
 *
 *   1. Register GratisFactory + CredisFactory as liquidity targets
 *   2. Compute user + vault deficits (targets: 1,000 / 10,000 tokens)
 *   3. Ensure holder has enough tokens (mint via owner if short)
 *   4. Transfer to user (from holder) if user deficit > 0
 *   5. Deposit to vault (from holder) if vault deficit > 0
 *
 * Usage: npx tsx src/0-setup-erc20.ts [envName]
 */

import { ethers, Wallet } from "ethers";
import { IERC20__factory, IVaultRouter__factory } from "./contracts/index.js";
import {
  DEFAULT_CREDIS_FACTORY_ADDRESS,
  DEFAULT_ENV,
  DEFAULT_GRATIS_FACTORY_ADDRESS,
  formatToken,
  fetchTokenMeta,
  loadEnv,
  requireEnv,
} from "./utils.js";

const USER_TARGET = 1_000_000_000n;    // 1,000 tokens (6 decimals)
const VAULT_TARGET = 10_000_000_000n;  // 10,000 tokens (6 decimals)
const INBOUND_FLOW = 3;                 // StablesSource.CredisCostAmount
const CREDIS_TARGET = 1;                // StablesTarget.Credis

const envName = process.argv[2] || DEFAULT_ENV;
const { envPath } = loadEnv(import.meta.url, envName, { deploymentEnv: true });

const rpcUrl = requireEnv("RPC_URL", envPath);
const ownerPrivateKey = requireEnv("PRIVATE_KEY", envPath);
const holderPrivateKey = requireEnv("ERC20_HOLDER_PRIVATE_KEY", envPath);
const userAddress = requireEnv("USER_ADDRESS", envPath);
const erc20Address = requireEnv("ERC20_ADDRESS", envPath);
const vaultRouterAddress = requireEnv("VAULT_ROUTER_ADDRESS", envPath);
const gratisFactoryAddress = process.env["GRATIS_FACTORY_ADDRESS"] || DEFAULT_GRATIS_FACTORY_ADDRESS;
const credisFactoryAddress = process.env["CREDIS_FACTORY_ADDRESS"] || DEFAULT_CREDIS_FACTORY_ADDRESS;

async function main() {
  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const ownerWallet = new Wallet(ownerPrivateKey, provider);
  const holderWallet = new Wallet(holderPrivateKey, provider);

  const tokenAsOwner = IERC20__factory.connect(erc20Address, ownerWallet);
  const tokenAsHolder = IERC20__factory.connect(erc20Address, holderWallet);
  // MockUSD-specific mint() is outside the IERC20 surface; bind it inline so the
  // setup flow can still top up the holder against a mintable test token.
  const mintableToken = new ethers.Contract(
    erc20Address,
    ["function mint(address to, uint256 amount)"],
    ownerWallet,
  );
  const vaultRouterAsOwner = IVaultRouter__factory.connect(vaultRouterAddress, ownerWallet);
  const vaultRouterAsHolder = IVaultRouter__factory.connect(vaultRouterAddress, holderWallet);

  const { decimals: tokenDecimals, symbol: tokenSymbol } = await fetchTokenMeta(tokenAsOwner);
  const fmt = (v: bigint) => formatToken(v, tokenDecimals, tokenSymbol);

  console.log("=== Setup ERC20 ===");
  console.log(`Env:            ${envName}`);
  console.log(`RPC:            ${rpcUrl}`);
  console.log(`Owner:          ${ownerWallet.address}`);
  console.log(`Holder:         ${holderWallet.address}`);
  console.log(`User:           ${userAddress}`);
  console.log(`ERC20:          ${erc20Address} (${tokenSymbol}, ${tokenDecimals} decimals)`);
  console.log(`Vault Router: ${vaultRouterAddress}`);

  // Both Credis precompiles pull assets out of the vault, which is exactly what
  // the liquidity-target registry authorizes: GratisFactory reserves the credit
  // at pledge time and CredisFactory releases that reservation at requestCredis.
  // Neither is seeded at genesis, so without this every pledgeGratis reverts.
  //
  // Runs before the deficit check below, which returns early on an already
  // funded chain — registration is a prerequisite, not a top-up.
  const resolveRouterOwnerSigner = await routerOwnerSignerResolver(provider, ownerWallet, holderWallet);

  console.log("\n[1] Registering liquidity targets...");
  const targetsCount = await vaultRouterAsOwner.liquidityTargetsCount();
  const registeredTargets = new Set<string>();
  for (let i = 0n; i < targetsCount; i++) {
    const { targetAddress } = await vaultRouterAsOwner.liquidityTargetAt(i);
    registeredTargets.add(targetAddress.toLowerCase());
  }

  for (const [label, address] of [
    ["GratisFactory", gratisFactoryAddress],
    ["CredisFactory", credisFactoryAddress],
  ] as const) {
    if (registeredTargets.has(address.toLowerCase())) {
      console.log(`    ${label} already registered as liquidity target`);
      continue;
    }
    const signer = resolveRouterOwnerSigner("register a liquidity target");
    const tx = await IVaultRouter__factory
      .connect(vaultRouterAddress, signer)
      .addLiquidityTarget(address, CREDIS_TARGET);
    await tx.wait();
    console.log(`    Registered ${label} (${address}) as liquidity target`);
  }

  // ── Step 2: Compute deficits ──────────────────────────────────────────────

  console.log("\n[2] Computing deficits...");
  const userBalance = await tokenAsOwner.balanceOf(userAddress);
  const underlyingVaultAddr = await vaultRouterAsOwner.assetVaultAt(erc20Address, 0);
  const vaultBalance = await tokenAsOwner.balanceOf(underlyingVaultAddr);

  const userDeficit = userBalance >= USER_TARGET ? 0n : USER_TARGET - userBalance;
  const vaultDeficit = vaultBalance >= VAULT_TARGET ? 0n : VAULT_TARGET - vaultBalance;
  const totalNeeded = userDeficit + vaultDeficit;

  console.log(`    User balance:  ${fmt(userBalance)} (target ${fmt(USER_TARGET)})`);
  console.log(`    Vault balance: ${fmt(vaultBalance)} (target ${fmt(VAULT_TARGET)})`);
  console.log(`    User deficit:  ${fmt(userDeficit)}`);
  console.log(`    Vault deficit: ${fmt(vaultDeficit)}`);

  if (totalNeeded === 0n) {
    console.log("\nSufficient — skipping");
    console.log("\n=== Setup ERC20 complete ===");
    return;
  }

  // ── Step 3: Ensure holder has enough tokens ───────────────────────────────

  console.log("\n[3] Checking holder ERC20 balance...");
  const holderBalance = await tokenAsOwner.balanceOf(holderWallet.address);
  console.log(`    Current: ${fmt(holderBalance)}`);

  if (holderBalance < totalNeeded) {
    const mintAmount = totalNeeded - holderBalance;
    const tx = await mintableToken.mint(holderWallet.address, mintAmount);
    await tx.wait();
    const newBal = await tokenAsOwner.balanceOf(holderWallet.address);
    console.log(`    Minted ${fmt(mintAmount)} → holder balance: ${fmt(newBal)}`);
  } else {
    console.log("    Sufficient — skipping mint");
  }

  // ── Step 4: Transfer to user ──────────────────────────────────────────────

  if (userDeficit > 0n) {
    console.log("\n[4] Transferring to user...");
    const tx = await tokenAsHolder.transfer(userAddress, userDeficit);
    await tx.wait();
    const newBal = await tokenAsOwner.balanceOf(userAddress);
    console.log(`    Sent ${fmt(userDeficit)} → user balance: ${fmt(newBal)}`);
  } else {
    console.log("\n[4] User balance sufficient — skipping transfer");
  }

  // ── Step 5: Deposit to vault ──────────────────────────────────────────────

  if (vaultDeficit > 0n) {
    console.log("\n[5] Depositing to vault...");

    let alreadyRegistered = false;
    const sourcesCount = await vaultRouterAsOwner.liquiditySourcesCount();
    for (let i = 0n; i < sourcesCount; i++) {
      const { sourceAddress } = await vaultRouterAsOwner.liquiditySourceAt(i);
      if (sourceAddress.toLowerCase() === holderWallet.address.toLowerCase()) {
        alreadyRegistered = true;
        break;
      }
    }

    if (alreadyRegistered) {
      console.log(`    Holder already registered as liquidity source`);
    } else {
      const signerForRegistration = resolveRouterOwnerSigner("register holder as liquidity source");
      const vaultRouterForRegistration = IVaultRouter__factory.connect(vaultRouterAddress, signerForRegistration);
      const setFlowTx = await vaultRouterForRegistration.addLiquiditySource(holderWallet.address, INBOUND_FLOW);
      await setFlowTx.wait();
      console.log(`    Registered holder as liquidity source (signed by ${signerForRegistration.address})`);
    }

    const approveTx = await tokenAsHolder.approve(vaultRouterAddress, vaultDeficit);
    await approveTx.wait();
    const depositTx = await vaultRouterAsHolder.deposit(erc20Address, vaultDeficit);
    await depositTx.wait();
    const newBal = await tokenAsOwner.balanceOf(underlyingVaultAddr);
    console.log(`    Deposited ${fmt(vaultDeficit)} → vault balance: ${fmt(newBal)}`);
  } else {
    console.log("\n[5] Vault balance sufficient — skipping deposit");
  }

  console.log("\n=== Setup ERC20 complete ===");
}

/**
 * Reads the vault router's owner once and returns a picker for whichever of the
 * two configured wallets holds it. Every `add*` on the router is `onlyOwner`, and
 * which key that is varies by deployment, so both registration paths share this.
 */
async function routerOwnerSignerResolver(
  provider: ethers.JsonRpcProvider,
  ownerWallet: Wallet,
  holderWallet: Wallet,
): Promise<(purpose: string) => Wallet> {
  const ownable = new ethers.Contract(
    vaultRouterAddress,
    ["function owner() view returns (address)"],
    provider,
  );
  const routerOwner: string = await ownable.owner();

  return (purpose: string) => {
    if (routerOwner.toLowerCase() === ownerWallet.address.toLowerCase()) {
      return ownerWallet;
    }
    if (routerOwner.toLowerCase() === holderWallet.address.toLowerCase()) {
      return holderWallet;
    }
    throw new Error(
      `VaultRouter owner ${routerOwner} is neither PRIVATE_KEY (${ownerWallet.address}) nor ERC20_HOLDER_PRIVATE_KEY (${holderWallet.address}); cannot ${purpose}`,
    );
  };
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
