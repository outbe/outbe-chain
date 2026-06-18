import { task } from "hardhat/config";
import { Options } from "@layerzerolabs/lz-v2-utilities";
import { createPublicClient, createWalletClient, getContract, http } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { getEnvRpcAndPk, makeChain } from "../../scripts/shared/layerzero.js";
import { getNetworkName } from "../../scripts/shared/taskUtils.js";
import { loadAbi } from "../../scripts/shared/abi.js";

// EID to network name mapping for common testnets/mainnets
const EID_TO_NETWORK: Record<number, string> = {
  40245: "base-sepolia",
  40102: "bsc-testnet",
  40231: "arbitrum-sepolia",
  40512: "outbe-priv",
  40712: "outbe-dev",
  30184: "base",
  30102: "bsc",
  30110: "arbitrum",
};

function endpointIdToNetwork(eid: number): string {
  return EID_TO_NETWORK[eid] || `unknown-${eid}`;
}

function getLayerZeroScanLink(txHash: string, isTestnet = false): string {
  const baseUrl = isTestnet
    ? "https://testnet.layerzeroscan.com"
    : "https://layerzeroscan.com";
  return `${baseUrl}/tx/${txHash}`;
}

function getBlockExplorerLink(
  networkName: string,
  txHash: string
): string | undefined {
  const explorers: Record<string, string> = {
    baseSepolia: "https://sepolia.basescan.org",
    bscTestnet: "https://testnet.bscscan.com",
    arbitrumSepolia: "https://sepolia.arbiscan.io",
  };

  const explorer = explorers[networkName];
  return explorer ? `${explorer}/tx/${txHash}` : undefined;
}

interface ONFT1155SendArgs {
  dstEid: string;
  tokenId: string;
  amount: string;
  to?: string;
  adapter: string;
  options?: string;
  composeMsg?: string;
}

const lazy =
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  <T extends (args: ONFT1155SendArgs, hre: any) => Promise<unknown>>(fn: T) =>
  async () => ({ default: fn });

const onft1155SendAction = async (args: ONFT1155SendArgs, hre: unknown) => {
  const networkName = getNetworkName(hre);
  const { rpc, pk } = getEnvRpcAndPk(networkName);
  if (!pk) throw new Error(`Private key required for ${networkName}`);
  const chain = makeChain(networkName, rpc);
  const account = privateKeyToAccount(pk as `0x${string}`);
  const transport = http(rpc);
  const publicClient = createPublicClient({ chain, transport });
  const walletClient = createWalletClient({ account, chain, transport });
  const signer = account.address;

  const dstEid = parseInt(args.dstEid, 10);
  const tokenId = BigInt(args.tokenId);
  const amount = BigInt(args.amount);

  if (!args.adapter) {
    console.error("❌ --adapter address is required");
    throw new Error("Missing adapter address");
  }

  console.log(
    `Initiating ONFT1155 transfer to ${endpointIdToNetwork(dstEid)}`
  );
  console.log(`Token ID: ${tokenId}, Amount: ${amount}`);
  console.log(`Destination EID: ${dstEid}`);

  console.log(`Using signer: ${signer}`);

  const recipient = (args.to || signer) as `0x${string}`;
  console.log(`Recipient: ${recipient}`);

  const adapterAddress = args.adapter as `0x${string}`;
  console.log(`ONFT1155Adapter: ${adapterAddress}`);

  // Get contracts using viem
  const adapter = getContract({
    address: adapterAddress,
    abi: loadAbi("ONFT1155Adapter"),
    client: { public: publicClient, wallet: walletClient },
  });

  const tokenAddress = (await adapter.read.token()) as `0x${string}`;
  console.log(`Token: ${tokenAddress}`);

  const token = getContract({
    address: tokenAddress,
    abi: loadAbi("IntexNFT1155"),
    client: { public: publicClient },
  });

  // Check token balance
  const balance = (await token.read.balanceOf([signer, tokenId])) as bigint;
  if (balance < amount) {
    console.error(`❌ Insufficient balance. Have ${balance.toString()}, need ${amount}`);
    throw new Error("Insufficient token balance");
  }

  // Prepare options
  const optionsHex = args.options || "0x";
  const extraOptions =
    optionsHex === "0x"
      ? Options.newOptions().addExecutorLzReceiveOption(200000, 0).toHex().toString()
      : optionsHex;

  // Pad recipient to bytes32
  const toBytes32 = `0x${recipient.slice(2).padStart(64, "0")}` as `0x${string}`;

  // Prepare send parameters
  const sendParam = {
    dstEid,
    to: toBytes32,
    tokenId,
    amount,
    extraOptions,
    composeMsg: (args.composeMsg || "0x") as `0x${string}`,
  };

  // Quote the gas cost
  console.log("Quoting gas cost for the send transaction...");
  let messagingFee: { nativeFee: bigint; lzTokenFee: bigint };
  try {
    messagingFee = (await adapter.read.quoteSend([sendParam, false])) as { nativeFee: bigint; lzTokenFee: bigint };
    const nativeFeeEth = Number(messagingFee.nativeFee) / 1e18;
    console.log(`  Native fee: ${nativeFeeEth.toFixed(6)} ETH`);
    console.log(`  LZ token fee: ${messagingFee.lzTokenFee.toString()} LZ`);
  } catch (error) {
    console.error(
      `❌ Error quoting gas for network: ${endpointIdToNetwork(dstEid)}, Contract: ${adapterAddress}`
    );
    throw error;
  }

  // Check ETH balance
  const ethBalance = await publicClient.getBalance({ address: signer });
  if (ethBalance < messagingFee.nativeFee) {
    const needEth = Number(messagingFee.nativeFee) / 1e18;
    const haveEth = Number(ethBalance) / 1e18;
    console.error(
      `❌ Insufficient ETH. Need ${needEth.toFixed(6)}, have ${haveEth.toFixed(6)}`
    );
    throw new Error("Insufficient ETH balance");
  }

  // Send the tokens
  console.log("Sending the tokens transaction...");
  let txHash: `0x${string}`;
  try {
    txHash = (await adapter.write.send([sendParam, messagingFee, signer], {
      value: messagingFee.nativeFee,
    })) as `0x${string}`;
    console.log(`  Transaction hash: ${txHash}`);
  } catch (error) {
    console.error(
      `❌ Error sending transaction to network: ${endpointIdToNetwork(dstEid)}, Contract: ${adapterAddress}`
    );
    throw error;
  }

  // Success messaging and links
  console.log(
    `✅ Successfully sent ${amount} tokens (ID: ${tokenId}) to ${endpointIdToNetwork(dstEid)}`
  );
  console.log(`  Transaction hash: ${txHash}`);

  // Get and display LayerZero scan link
  const isTestnet = dstEid >= 40_000 && dstEid < 50_000;
  const scanLink = getLayerZeroScanLink(txHash, isTestnet);
  console.log(
    `✅ LayerZero Scan link for tracking cross-chain delivery: ${scanLink}`
  );

  return {
    txHash,
    scanLink,
  };
};

const onft1155Send = task(
  "lz:onft1155:send",
  "Sends ERC1155 tokens cross-chain using ONFT1155Adapter"
)
  .addOption({
    name: "adapter",
    description: "ONFT1155Adapter contract address (required)",
    defaultValue: "",
  })
  .addOption({
    name: "dstEid",
    description: "Destination endpoint ID",
    defaultValue: "",
  })
  .addOption({
    name: "tokenId",
    description: "Token ID to transfer",
    defaultValue: "",
  })
  .addOption({
    name: "amount",
    description: "Amount to transfer",
    defaultValue: "",
  })
  .addOption({
    name: "to",
    description: "Recipient address (defaults to sender)",
    defaultValue: "",
  })
  .addOption({
    name: "options",
    description: "Execution options (hex string)",
    defaultValue: "0x",
  })
  .addOption({
    name: "composeMsg",
    description: "Composed message (hex string)",
    defaultValue: "0x",
  })
  .setAction(lazy(onft1155SendAction));

export const onft1155Tasks = [onft1155Send.build()];
