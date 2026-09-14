/**
 * 0-setup-native.ts
 *
 * Ensures user and CCA have sufficient native (COEN) balances.
 *
 *   1. Ensure user has native balance (funds gas + the EntryPoint deposit step 5 needs)
 *   2. Ensure CCA has native balance (gas, its EntryPoint deposit, and the COEN stake
 *      requestCredis takes) and register its 1 billion COEN bond.
 *
 * Usage: npx tsx src/0-setup-native.ts [envName]
 */

import { ethers, Wallet } from "ethers";
import { ICca__factory } from "./contracts/index.js";
import { coen, formatCoen, DEFAULT_ENV, loadEnv, requireEnv } from "./utils.js";

// Registration escrow is separate from the COEN spent on individual positions.
const BOND_REQUIREMENT = coen("1000000000");
const CCA_GAS_AND_POSITION_FUNDS = coen("50");
const CCA_REGISTRY = "0x0000000000000000000000000000000000001011";
const USER_FUND_NATIVE = coen("100");

const envName = process.argv[2] || DEFAULT_ENV;
const { envPath } = loadEnv(import.meta.url, envName, { deploymentEnv: true });

const rpcUrl = requireEnv("RPC_URL", envPath);
const ownerPrivateKey = requireEnv("PRIVATE_KEY", envPath);
const userAddress = requireEnv("USER_ADDRESS", envPath);
const ccaAddress = requireEnv("CCA_ADDRESS", envPath);

async function main() {
  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const ownerWallet = new Wallet(ownerPrivateKey, provider);
  const ccaWallet = new Wallet(requireEnv("CCA_PRIVATE_KEY", envPath), provider);
  if (ccaWallet.address.toLowerCase() !== ccaAddress.toLowerCase()) {
    throw new Error("CCA_PRIVATE_KEY does not match CCA_ADDRESS");
  }
  const registry = ICca__factory.connect(CCA_REGISTRY, ccaWallet);
  const registration = await registry.getCca(ccaAddress);
  if (registration.state === 2n) {
    throw new Error("CCA has a pending unbond; claim it before registering again");
  }
  const remainingBond = registration.bondedAmount < BOND_REQUIREMENT
    ? BOND_REQUIREMENT - registration.bondedAmount : 0n;
  const requiredCcaBalance = remainingBond + CCA_GAS_AND_POSITION_FUNDS;

  console.log("=== Setup Native ===");
  console.log(`Env:   ${envName}`);
  console.log(`RPC:   ${rpcUrl}`);
  console.log(`Owner: ${ownerWallet.address}`);
  console.log(`User:  ${userAddress}`);
  console.log(`CCA:   ${ccaAddress}`);

  // -- Step 1: Ensure user has native balance --------------------------------

  console.log("\n[1] Checking user native balance...");
  const userNative = await provider.getBalance(userAddress);
  console.log(`    Current: ${formatCoen(userNative)} COEN`);

  if (userNative < USER_FUND_NATIVE) {
    const tx = await ownerWallet.sendTransaction({ to: userAddress, value: USER_FUND_NATIVE });
    await tx.wait();
    console.log(`    Funded user with ${formatCoen(USER_FUND_NATIVE)} COEN (tx: ${tx.hash})`);
  } else {
    console.log("    Sufficient - skipping");
  }

  // -- Step 2: Ensure CCA has native balance ---------------------------------

  console.log("\n[2] Checking CCA native balance...");
  const ccaNative = await provider.getBalance(ccaAddress);
  console.log(`    Current: ${formatCoen(ccaNative)} COEN`);

  if (ccaNative < requiredCcaBalance) {
    const shortfall = requiredCcaBalance - ccaNative;
    const tx = await ownerWallet.sendTransaction({ to: ccaAddress, value: shortfall });
    await tx.wait();
    console.log(`    Funded CCA with ${formatCoen(shortfall)} COEN (tx: ${tx.hash})`);
  } else {
    console.log("    Sufficient - skipping");
  }

  if (remainingBond > 0n) {
    const tx = await registry.bond({ value: remainingBond });
    await tx.wait();
    console.log(`    Bonded ${formatCoen(remainingBond)} COEN (tx: ${tx.hash})`);
  }
  console.log("\n=== Setup Native complete ===");
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
