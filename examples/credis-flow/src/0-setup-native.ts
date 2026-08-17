/**
 * 0-setup-native.ts
 *
 * Ensures user and CCA have sufficient native (COEN) balances, and that the CCA
 * is registered as an origination agent.
 *
 *   1. Ensure user has native balance (10 COEN if zero)
 *   2. Ensure CCA has native balance (5 COEN if < 5 COEN)
 *   3. Register the CCA so it may originate positions
 *
 * Usage: npx tsx src/0-setup-native.ts [envName]
 */

import { ethers, Wallet } from "ethers";
import { ICca__factory } from "./contracts/index.js";
import { DEFAULT_CCA_REGISTRY_ADDRESS, DEFAULT_ENV, loadEnv, requireEnv } from "./utils.js";

const CCA_MIN_NATIVE = ethers.parseEther("5");      // 5 COEN
const USER_FUND_NATIVE = ethers.parseEther("10");    // 10 COEN

const envName = process.argv[2] || DEFAULT_ENV;
const { envPath } = loadEnv(import.meta.url, envName, { deploymentEnv: true });

const rpcUrl = requireEnv("RPC_URL", envPath);
const ownerPrivateKey = requireEnv("PRIVATE_KEY", envPath);
const userAddress = requireEnv("USER_ADDRESS", envPath);
const ccaAddress = requireEnv("CCA_ADDRESS", envPath);
const ccaPrivateKey = requireEnv("CCA_PRIVATE_KEY", envPath);
const ccaRegistryAddress = process.env["CCA_REGISTRY_ADDRESS"] || DEFAULT_CCA_REGISTRY_ADDRESS;

async function main() {
  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const ownerWallet = new Wallet(ownerPrivateKey, provider);

  console.log("=== Setup Native ===");
  console.log(`Env:   ${envName}`);
  console.log(`RPC:   ${rpcUrl}`);
  console.log(`Owner: ${ownerWallet.address}`);
  console.log(`User:  ${userAddress}`);
  console.log(`CCA:   ${ccaAddress}`);

  // ── Step 1: Ensure user has native balance ────────────────────────────────

  console.log("\n[1] Checking user native balance...");
  const userNative = await provider.getBalance(userAddress);
  console.log(`    Current: ${ethers.formatEther(userNative)} COEN`);

  if (userNative === 0n) {
    const tx = await ownerWallet.sendTransaction({ to: userAddress, value: USER_FUND_NATIVE });
    await tx.wait();
    console.log(`    Funded user with 10 COEN (tx: ${tx.hash})`);
  } else {
    console.log("    Sufficient — skipping");
  }

  // ── Step 2: Ensure CCA has native balance ─────────────────────────────────

  console.log("\n[2] Checking CCA native balance...");
  const ccaNative = await provider.getBalance(ccaAddress);
  console.log(`    Current: ${ethers.formatEther(ccaNative)} COEN`);

  if (ccaNative < CCA_MIN_NATIVE) {
    const tx = await ownerWallet.sendTransaction({ to: ccaAddress, value: CCA_MIN_NATIVE });
    await tx.wait();
    console.log(`    Funded CCA with 5 COEN (tx: ${tx.hash})`);
  } else {
    console.log("    Sufficient — skipping");
  }

  // ── Step 3: Register the CCA ──────────────────────────────────────────────

  // `requestCredis` rejects a caller that is not a registered agent, so without
  // this the whole flow stops at step 3. Registration is permissionless and the
  // agent signs for itself, which is why it follows the funding above.
  console.log("\n[3] Checking CCA registration...");
  const ccaWallet = new Wallet(ccaPrivateKey, provider);
  if (ccaWallet.address.toLowerCase() !== ccaAddress.toLowerCase()) {
    throw new Error(
      `CCA_PRIVATE_KEY is for ${ccaWallet.address}, but CCA_ADDRESS is ${ccaAddress}`,
    );
  }

  // `getCca` reverts for an unregistered agent; `canOriginate` is false for both
  // that and for a quarantined one, so the two are separated here — re-running
  // register() on a quarantined agent would just revert "already registered".
  const registry = ICca__factory.connect(ccaRegistryAddress, ccaWallet);
  const isRegistered = await registry.getCca(ccaAddress).then(() => true, () => false);

  if (!isRegistered) {
    const tx = await registry.register();
    await tx.wait();
    console.log(`    Registered CCA (tx: ${tx.hash})`);
  } else if (await registry.canOriginate(ccaAddress)) {
    console.log("    Already registered — skipping");
  } else {
    const multiplier = await registry.multiplierOf(ccaAddress);
    throw new Error(
      `CCA ${ccaAddress} is registered but quarantined (multiplier ` +
        `${ethers.formatEther(multiplier)} < 0.5); requestCredis will reject it`,
    );
  }

  console.log("\n=== Setup Native complete ===");
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
