import { ethers, toBigInt } from "ethers";
import {
  IGratis__factory,
  ICredis__factory,
  SmartAccountFactory__factory,
  IERC20__factory,
  ITokenBundle__factory,
  IVaultProvider__factory,
} from "./contracts/index.js";
import {
  DEFAULT_GRATIS_ADDRESS,
  DEFAULT_GRATIS_FACTORY_ADDRESS,
  DEFAULT_CREDIS_FACTORY_ADDRESS,
  DEFAULT_CREDIS_ADDRESS,
  formatToken,
  formatTokenMeta,
  fetchTokenMeta,
  TokenMeta,
  DEFAULT_ENV,
  loadEnv,
  requireEnv, formatTokenMeta2,
} from "./utils.js";
import { deriveGratisKeys, decryptBalance, decryptPledged, type GratisKeys } from "./confidential.js";

const SALT = 0n;

// Parse CLI args: [envName]
const envName = process.argv[2] || DEFAULT_ENV;

// Load env files
const { envPath } = loadEnv(import.meta.url, envName, { deploymentEnv: true });

const rpcUrl = requireEnv("RPC_URL", envPath);
const userAddress = requireEnv("USER_ADDRESS", envPath);
const ccaAddress = requireEnv("CCA_ADDRESS", envPath);
const gratisAddress = process.env["GRATIS_ADDRESS"] || DEFAULT_GRATIS_ADDRESS;
const gratisFactoryAddress = process.env["GRATIS_FACTORY_ADDRESS"] || DEFAULT_GRATIS_FACTORY_ADDRESS;
const credisFactoryAddress = process.env["CREDIS_FACTORY_ADDRESS"] || DEFAULT_CREDIS_FACTORY_ADDRESS;
const credisAddress = process.env["CREDIS_ADDRESS"] || DEFAULT_CREDIS_ADDRESS;
const smartAccountFactoryAddress = requireEnv("SMART_ACCOUNT_FACTORY_ADDRESS", envPath);
const bundleModulePluginAddress = requireEnv("BUNDLE_MODULE_PLUGIN_ADDRESS", envPath);
const erc20Address = requireEnv("ERC20_ADDRESS", envPath);
const vaultProviderAddress = requireEnv("VAULT_PROVIDER_ADDRESS", envPath);

function formatDate(timestamp: bigint): string {
  if (timestamp === 0n) return "N/A";
  return new Date(Number(timestamp) * 1000).toISOString();
}

async function main() {
  const provider = new ethers.JsonRpcProvider(rpcUrl);

  const gratis = IGratis__factory.connect(gratisAddress, provider);
  const credis = ICredis__factory.connect(credisAddress, provider);
  const saFactory = SmartAccountFactory__factory.connect(smartAccountFactoryAddress, provider);
  const token = IERC20__factory.connect(erc20Address, provider);
  const bundlePlugin = ITokenBundle__factory.connect(bundleModulePluginAddress, provider);
  const vaultProvider = IVaultProvider__factory.connect(vaultProviderAddress, provider);

  console.log("=== Credis Info ===");
  console.log(`Env:              ${envName}`);
  console.log(`RPC:              ${rpcUrl}`);
  console.log(`Gratis:           ${gratisAddress}  TotalSupply: ${(await gratis.totalSupply()).toString()}`);
  console.log(`GratisFactory:    ${gratisFactoryAddress}`);
  console.log(`CredisFactory:    ${credisFactoryAddress}`);
  console.log(`Credis:           ${credisAddress}`);
  console.log(`SA Factory:       ${smartAccountFactoryAddress}`);
  console.log(`Bundle Plugin:    ${bundleModulePluginAddress}`);
  console.log(`ERC20:            ${erc20Address}  TotalSupply: ${(await token.totalSupply()).toString()} ${await token.name()}`);
  console.log(`Vault Provider:   ${vaultProviderAddress}`);

  const [gratisMeta, erc20Meta] = await Promise.all([fetchTokenMeta(gratis), fetchTokenMeta(token)]);

  // The user's Gratis balances are confidential — fetch the view key via
  // `outbe_deriveGratisKeys` to decrypt them. (Null if the enclave/DKG isn't up.)
  let userKeys: GratisKeys | null = null;
  try {
    userKeys = await deriveGratisKeys(provider, userAddress);
  } catch (e) {
    console.warn(`\n(!) Could not fetch Gratis view key (${(e as Error).message}); balances shown as ciphertext.`);
  }

  const smartAccountAddr = await saFactory.getAccountAddress(
    userAddress,
    ccaAddress,
    [erc20Address],
    [vaultProviderAddress],
    SALT,
  );

  await printUserInfo(provider, gratis, token, gratisMeta, erc20Meta, userKeys);
  await printSmartAccountInfo(provider, token, bundlePlugin, smartAccountAddr, erc20Meta);
  await printCredisInfo(credis, smartAccountAddr, erc20Meta);
  await printCcaInfo(provider, token, erc20Meta);
  await printVaultProviderInfo(vaultProvider, token, erc20Address, erc20Meta);
}

async function printUserInfo(
  provider: ethers.JsonRpcProvider,
  gratis: ReturnType<typeof IGratis__factory.connect>,
  token: ReturnType<typeof IERC20__factory.connect>,
  gratisMeta: TokenMeta,
  erc20Meta: TokenMeta,
  keys: GratisKeys | null,
) {
  const [nativeBalance, erc20Balance, gratisBlob, pledgedBlob, pledgedTotal] = await Promise.all([
    provider.getBalance(userAddress),
    token.balanceOf(userAddress),
    gratis.balanceOf(userAddress),
    gratis.pledgedOf(userAddress),
    gratis.pledgedTotalSupply(),
  ]);

  const showGratis = keys
    ? formatTokenMeta(decryptBalance(keys.viewKey, userAddress, gratisBlob), gratisMeta)
    : `${gratisBlob} (ciphertext — need view key)`;
  const showPledged = keys
    ? formatTokenMeta(decryptPledged(keys.viewKey, userAddress, pledgedBlob), gratisMeta)
    : `${pledgedBlob} (ciphertext — need view key)`;

  console.log(`\n=== User: ${userAddress} ===`);
  console.log(`  Native balance:  ${ethers.formatEther(nativeBalance)} COEN`);
  console.log(`  ERC20 balance:   ${formatTokenMeta(erc20Balance, erc20Meta)}`);
  console.log(`  Gratis balance:  ${showGratis}   ${keys ? "(decrypted with view key)" : ""}`);
  console.log(`  Pledged gratis:  ${showPledged}`);
  console.log(`  Pledged total:   ${formatTokenMeta(pledgedTotal, gratisMeta)} (system-wide, plaintext aggregate)`);
}

async function printSmartAccountInfo(
  provider: ethers.JsonRpcProvider,
  token: ReturnType<typeof IERC20__factory.connect>,
  bundlePlugin: ReturnType<typeof ITokenBundle__factory.connect>,
  smartAccountAddr: string,
  erc20Meta: TokenMeta,
) {
  const code = await provider.getCode(smartAccountAddr);
  const deployed = code !== "0x";

  console.log(`\n=== User's Bundle Account: ${smartAccountAddr} ===`);
  console.log(`  Deployed:        ${deployed}`);

  if (!deployed) return;

  const [nativeBalance, erc20Balance, bundleBalance] = await Promise.all([
    provider.getBalance(smartAccountAddr),
    token.balanceOf(smartAccountAddr),
    bundlePlugin.balanceOf(smartAccountAddr, erc20Address).catch(() => 0n),
  ]);

  const bundleBalance2 = bundleBalance / toBigInt(2);
  const personalBalance = erc20Balance - bundleBalance;
  console.log(`  Native balance:  ${ethers.formatEther(nativeBalance)} COEN`);
  console.log(`  ERC20 balance (total):   ${formatTokenMeta(erc20Balance, erc20Meta)}`);
  console.log(`     Bundle:               ${formatTokenMeta(bundleBalance, erc20Meta)} (${formatTokenMeta2(bundleBalance2, erc20Meta)} + ${formatTokenMeta2(bundleBalance2, erc20Meta)})`);
  console.log(`     Personal:             ${formatTokenMeta(personalBalance, erc20Meta)}`);
}

async function printCredisInfo(
  credis: ReturnType<typeof ICredis__factory.connect>,
  smartAccountAddr: string,
  erc20Meta: TokenMeta,
) {
  const [positions, hasOverdue] = await Promise.all([
    credis.getPositionsByAddress(smartAccountAddr).catch(() => []),
    credis.hasOverdueAnadosis(smartAccountAddr).catch(() => false),
  ]);

  console.log(`\n=== Credis Positions (Bundle Account: ${smartAccountAddr}) ===`);
  console.log(`  Positions:       ${positions.length} (overdue: ${hasOverdue})`);

  for (const p of positions) {
    console.log(`    Position ${p.positionId} :`);
    console.log(`      totalAnadosisAmount: ${formatTokenMeta(p.totalAnadosisAmount, erc20Meta)}, outstanding: ${formatTokenMeta(p.outstandingAnadosisAmount, erc20Meta)}`);
    console.log(`      totalGratisAmount: ${formatToken(p.totalGratisAmount, 18, "GRATIS")}, outstandingGratisAmount: ${formatToken(p.outstandingGratisAmount, 18, "GRATIS")}`);
    console.log(`      created: ${formatDate(p.createdAt)}`);

    const anadosisList = await credis.getPositionAnadosis(p.positionId).catch(() => null);
    if (anadosisList) {
      for (const a of anadosisList) {
        const status = a.paidAt > 0n ? `paid at ${formatDate(a.paidAt)}` : "unpaid";
        console.log(`      anadosis #${a.anadosisNumber}: due ${formatDate(a.dueDate)}, amount: ${formatTokenMeta(a.anadosisAmount, erc20Meta)}, gratis: ${formatToken(a.gratisAmount, 18, "GRATIS")}, ${status}`);
      }
    }
  }
}

async function printCcaInfo(
  provider: ethers.JsonRpcProvider,
  token: ReturnType<typeof IERC20__factory.connect>,
  erc20Meta: TokenMeta,
) {
  const [nativeBalance, erc20Balance] = await Promise.all([
    provider.getBalance(ccaAddress),
    token.balanceOf(ccaAddress),
  ]);

  console.log(`\n=== CCA: ${ccaAddress} ===`);
  console.log(`  Native balance:  ${ethers.formatEther(nativeBalance)} COEN`);
  console.log(`  ERC20 balance:   ${formatTokenMeta(erc20Balance, erc20Meta)}`);
}

async function printVaultProviderInfo(
  vaultProvider: ReturnType<typeof IVaultProvider__factory.connect>,
  token: ReturnType<typeof IERC20__factory.connect>,
  assetAddress: string,
  erc20Meta: TokenMeta,
) {
  const underlyingVault = await vaultProvider.assetVaultAt(assetAddress, 0);
  const sharesBalance = await vaultProvider.sharesBalance(underlyingVault);
  const vaultErc20Balance = await token.balanceOf(underlyingVault);

  console.log(`\n=== Vault Provider: ${vaultProviderAddress} ===`);
  console.log(`  Underlying Outbe vault:  ${underlyingVault}`);
  console.log(`  Shares balance:          ${sharesBalance}`);
  console.log(`  Vault ERC20 bal:         ${formatTokenMeta(vaultErc20Balance, erc20Meta)}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
