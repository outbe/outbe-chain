import { ethers, Wallet } from "ethers";
import { IGratis__factory, IGratisFactory__factory } from "./contracts/index.js";
import {
  DEFAULT_GRATIS_ADDRESS,
  DEFAULT_GRATIS_FACTORY_ADDRESS,
  formatToken,
  formatTokenDiff,
  fetchTokenMeta,
  DEFAULT_ENV,
  loadEnv,
  requireEnv,
} from "./utils.js";
import {
  deriveGratisKeys,
  decryptBalance,
  decryptPledged,
  modifyMac,
  pledgeSecret,
  GratisOp,
} from "./confidential.js";
import { writeTicket } from "./ticket.js";

// CLI: [amountGratis] [envName]. Amount defaults to "1" GRATIS (raw amount —
// the denomination ladder is gone with the TEE migration).
const amountArg = process.argv[2] || "1";
const envName = process.argv[3] || DEFAULT_ENV;

const { envPath } = loadEnv(import.meta.url, envName);

const rpcUrl = requireEnv("RPC_URL", envPath);
const userPrivateKey = requireEnv("USER_PRIVATE_KEY", envPath);
const userAddress = requireEnv("USER_ADDRESS", envPath);
const gratisAddress = process.env["GRATIS_ADDRESS"] || DEFAULT_GRATIS_ADDRESS;
const gratisFactoryAddress = process.env["GRATIS_FACTORY_ADDRESS"] || DEFAULT_GRATIS_FACTORY_ADDRESS;

async function main() {
  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const wallet = new Wallet(userPrivateKey, provider);
  const gratis = IGratis__factory.connect(gratisAddress, wallet);
  const gratisFactory = IGratisFactory__factory.connect(gratisFactoryAddress, wallet);

  const gratisMeta = await fetchTokenMeta(gratis);
  const amount = ethers.parseUnits(amountArg, gratisMeta.decimals);
  const { chainId } = await provider.getNetwork();

  // Fetch the user's enclave-derived confidential keys (view + modify) so we can
  // read the encrypted balance and authorize the write. Signs an ownership proof.
  const keys = await deriveGratisKeys(wallet);

  const opNonce = await gratis.opNonceOf(userAddress);
  const balanceBefore = decryptBalance(keys.viewKey, userAddress, await gratis.balanceOf(userAddress));
  const pledgedBefore = decryptPledged(keys.viewKey, userAddress, await gratis.pledgedOf(userAddress));

  console.log("=== Pledge Gratis (confidential / TEE) ===");
  console.log(`Env:        ${envName} (${envPath})`);
  console.log(`RPC:        ${rpcUrl}`);
  console.log(`User:       ${userAddress}`);
  console.log(`Gratis:     ${gratisAddress} (${gratisMeta.symbol})`);
  console.log(`Factory:    ${gratisFactoryAddress}`);
  console.log(`Amount:     ${formatToken(amount, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`Op-nonce:   ${opNonce}`);

  console.log("\n=== State BEFORE (decrypted with the view key) ===");
  console.log(`  Balance:  ${formatToken(balanceBefore, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledged:  ${formatToken(pledgedBefore, gratisMeta.decimals, gratisMeta.symbol)}`);

  if (balanceBefore < amount) {
    console.error(
      `Insufficient Gratis balance: have ${formatToken(balanceBefore, gratisMeta.decimals, gratisMeta.symbol)}, need ${formatToken(amount, gratisMeta.decimals, gratisMeta.symbol)}`,
    );
    process.exit(1);
  }

  // Authorize the pledge with the modify key, bound to the current op-nonce.
  const mac = modifyMac(keys.modifyKey, userAddress, GratisOp.Pledge, amount, opNonce, chainId);

  console.log("\nSending pledgeGratis(amount, mac, opNonce)...");
  const tx = await gratisFactory.pledgeGratis(amount, mac, opNonce);
  console.log(`  TX hash: ${tx.hash}`);
  const receipt = await tx.wait();
  if (!receipt) throw new Error("pledgeGratis tx receipt missing");
  console.log(`  Block:   ${receipt.blockNumber}`);

  // Capture the confidential pledge handle from the GratisPledged event.
  const factoryIface = IGratisFactory__factory.createInterface();
  const pledged = receipt.logs
    .filter((l) => l.address.toLowerCase() === gratisFactoryAddress.toLowerCase())
    .map((l) => {
      try {
        return factoryIface.parseLog({ topics: l.topics as string[], data: l.data });
      } catch {
        return null;
      }
    })
    .find((p) => p?.name === "GratisPledged");
  if (!pledged) throw new Error("GratisPledged event not found in receipt");
  const handle = pledged.args.pledgeHandle as string;

  // The bearer secret the user hands to the CCA to request credis later.
  const secret = pledgeSecret(keys.modifyKey, handle);

  const balanceAfter = decryptBalance(keys.viewKey, userAddress, await gratis.balanceOf(userAddress));
  const pledgedAfter = decryptPledged(keys.viewKey, userAddress, await gratis.pledgedOf(userAddress));

  console.log("\n=== State AFTER (decrypted with the view key) ===");
  console.log(`  Balance:       ${formatToken(balanceAfter, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledged:       ${formatToken(pledgedAfter, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledge handle: ${handle}`);

  console.log("\n=== CHANGES ===");
  console.log(`  Balance:  ${formatTokenDiff(balanceAfter - balanceBefore, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledged:  ${formatTokenDiff(pledgedAfter - pledgedBefore, gratisMeta.decimals, gratisMeta.symbol)}`);

  const ticketPath = writeTicket({
    pledgeHandle: handle,
    pledgeSecret: ethers.hexlify(secret),
    amount: amount.toString(),
    opNonce: Number(opNonce),
    blockNumber: receipt.blockNumber,
    txHash: receipt.hash,
    chainId: chainId.toString(),
    createdAt: new Date().toISOString(),
  });

  console.log(`\nTicket written: ${ticketPath}`);
  console.log("Hand the pledgeSecret to the CCA, then run `npm run request-credis`.");
  console.log("(Or `npm run unpledge-gratis-fast` to directly reclaim this unspent pledge.)");
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
