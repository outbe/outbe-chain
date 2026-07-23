import { ethers, Wallet } from "ethers";
import { IGratis__factory, IGratisFactory__factory, IPromis__factory } from "./contracts/index.js";
import {
  DEFAULT_GRATIS_ADDRESS,
  DEFAULT_GRATIS_FACTORY_ADDRESS,
  DEFAULT_PROMIS_ADDRESS,
  formatToken,
  formatTokenDiff,
  fetchTokenMeta,
  DEFAULT_ENV,
  loadEnv,
  requireEnv,
} from "./utils.js";
import { deriveGratisKeys, decryptBalance, modifyMac, GratisOp } from "./confidential.js";

// CLI: [amountPromis] [envName]. Amount defaults to "1000" (the seeded Promis
// balance). Converts public Promis into confidential Gratis 1:1 so the user has
// a real, enclave-encrypted Gratis balance to pledge — Gratis can't be
// plaintext-seeded at genesis anymore (it's TEE-encrypted at rest).
const amountArg = process.argv[2] || "1000";
const envName = process.argv[3] || DEFAULT_ENV;

const { envPath } = loadEnv(import.meta.url, envName);

const rpcUrl = requireEnv("RPC_URL", envPath);
const userPrivateKey = requireEnv("USER_PRIVATE_KEY", envPath);
const userAddress = requireEnv("USER_ADDRESS", envPath);
const gratisAddress = process.env["GRATIS_ADDRESS"] || DEFAULT_GRATIS_ADDRESS;
const promisAddress = process.env["PROMIS_ADDRESS"] || DEFAULT_PROMIS_ADDRESS;
const gratisFactoryAddress = process.env["GRATIS_FACTORY_ADDRESS"] || DEFAULT_GRATIS_FACTORY_ADDRESS;

async function main() {
  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const wallet = new Wallet(userPrivateKey, provider);
  const gratis = IGratis__factory.connect(gratisAddress, wallet);
  const promis = IPromis__factory.connect(promisAddress, wallet);
  const gratisFactory = IGratisFactory__factory.connect(gratisFactoryAddress, wallet);

  const gratisMeta = await fetchTokenMeta(gratis);
  const promisMeta = await fetchTokenMeta(promis);
  // Conversion is 1:1 on raw amounts; both tokens are 18-dec.
  const amount = ethers.parseUnits(amountArg, promisMeta.decimals);
  const { chainId } = await provider.getNetwork();

  // Fetch the user's enclave-derived confidential keys (view + modify) so we can
  // read the encrypted Gratis balance and authorize the mint. Signs an ownership proof.
  const keys = await deriveGratisKeys(wallet);

  const opNonce = await gratis.opNonceOf(userAddress);
  const promisBefore = await promis.balanceOf(userAddress);
  const gratisBefore = decryptBalance(keys.viewKey, userAddress, await gratis.balanceOf(userAddress));

  console.log("=== Setup Gratis via Promis→Gratis conversion (confidential / TEE) ===");
  console.log(`Env:            ${envName} (${envPath})`);
  console.log(`RPC:            ${rpcUrl}`);
  console.log(`User:           ${userAddress}`);
  console.log(`Promis:         ${promisAddress} (${promisMeta.symbol})`);
  console.log(`GratisFactory:  ${gratisFactoryAddress}`);
  console.log(`Gratis:         ${gratisAddress} (${gratisMeta.symbol})`);
  console.log(`Amount:         ${formatToken(amount, promisMeta.decimals, promisMeta.symbol)}`);
  console.log(`Op-nonce:       ${opNonce}`);

  console.log("\n=== State BEFORE ===");
  console.log(`  Promis:   ${formatToken(promisBefore, promisMeta.decimals, promisMeta.symbol)}`);
  console.log(`  Gratis:   ${formatToken(gratisBefore, gratisMeta.decimals, gratisMeta.symbol)} (decrypted)`);

  if (promisBefore < amount) {
    console.error(
      `Insufficient Promis to convert: have ${formatToken(promisBefore, promisMeta.decimals, promisMeta.symbol)}, ` +
        `need ${formatToken(amount, promisMeta.decimals, promisMeta.symbol)}. ` +
        `Is promis_balances seeded for this account at genesis?`,
    );
    process.exit(1);
  }

  // Authorize the confidential Gratis mint with the modify key, bound to the
  // current op-nonce. mineFromPromis burns Promis and mints Gratis via the
  // enclave's `mine` op, so the MAC uses GratisOp.Mine.
  const mac = modifyMac(keys.modifyKey, userAddress, GratisOp.Mine, amount, opNonce, chainId);

  console.log("\nSending mineFromPromis(amount, mac, opNonce)...");
  const tx = await gratisFactory.mineFromPromis(amount, mac, opNonce);
  console.log(`  TX hash: ${tx.hash}`);
  const receipt = await tx.wait();
  if (!receipt) throw new Error("mineFromPromis tx receipt missing");
  console.log(`  Block:   ${receipt.blockNumber}`);

  const promisAfter = await promis.balanceOf(userAddress);
  const gratisAfter = decryptBalance(keys.viewKey, userAddress, await gratis.balanceOf(userAddress));

  console.log("\n=== State AFTER (Gratis decrypted with the view key) ===");
  console.log(`  Promis:   ${formatToken(promisAfter, promisMeta.decimals, promisMeta.symbol)}`);
  console.log(`  Gratis:   ${formatToken(gratisAfter, gratisMeta.decimals, gratisMeta.symbol)}`);

  console.log("\n=== CHANGES ===");
  console.log(`  Promis:   ${formatTokenDiff(promisAfter - promisBefore, promisMeta.decimals, promisMeta.symbol)}`);
  console.log(`  Gratis:   ${formatTokenDiff(gratisAfter - gratisBefore, gratisMeta.decimals, gratisMeta.symbol)}`);

  console.log("\nDone. Run `npm run pledge-gratis` to pledge some of this Gratis.");
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
