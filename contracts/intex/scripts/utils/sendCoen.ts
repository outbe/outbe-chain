/**
 * Send native COEN from one wallet to another on Outbe Devnet or Privnet
 *
 * Usage: FROM_PRIVATE_KEY=0x... tsx scripts/sendOutbeCoen.ts <toAddress> <amountInCoen> [network]
 *   network: devnet (default) | privnet
 */

import { createWalletClient, createPublicClient, http, parseEther, formatEther } from "viem";
import { privateKeyToAccount } from "viem/accounts";

const OUTBE_NETWORKS = {
  devnet: {
    name: "Outbe Devnet",
    chainId: 424242,
    rpc: "https://eth.d.outbe.net/",
    symbol: "COEN",
  },
  privnet: {
    name: "Outbe Privnet",
    chainId: 512512,
    rpc: "https://eth.p.outbe.net",
    symbol: "COEN",
  },
} as const;

type NetworkKey = keyof typeof OUTBE_NETWORKS;

async function main() {
  const privateKey = process.env.FROM_PRIVATE_KEY;
  if (!privateKey) {
    console.error("Error: Set FROM_PRIVATE_KEY environment variable (sender's private key)");
    process.exit(1);
  }

  const toAddress = process.argv[2];
  const amountStr = process.argv[3];
  const networkArg = (process.argv[4] ?? "devnet").toLowerCase();

  if (!toAddress?.startsWith("0x") || toAddress.length !== 42) {
    console.error("Usage: FROM_PRIVATE_KEY=0x... tsx scripts/sendOutbeCoen.ts <toAddress> <amountInCoen> [network]");
    console.error("  network: devnet (default) | privnet");
    console.error("Example: FROM_PRIVATE_KEY=0x... tsx scripts/sendOutbeCoen.ts 0x7099... 10 devnet");
    process.exit(1);
  }

  if (!amountStr || isNaN(parseFloat(amountStr)) || parseFloat(amountStr) <= 0) {
    console.error("Error: Provide a valid positive amount in COEN");
    process.exit(1);
  }

  const network = OUTBE_NETWORKS[networkArg as NetworkKey];
  if (!network) {
    console.error(`Error: Unknown network "${networkArg}". Use devnet or privnet`);
    process.exit(1);
  }

  const amountWei = parseEther(amountStr);
  const pk = privateKey.startsWith("0x") ? (privateKey as `0x${string}`) : (`0x${privateKey}` as `0x${string}`);
  const account = privateKeyToAccount(pk);

  const transport = http(network.rpc, { timeout: 15_000 });

  const publicClient = createPublicClient({ transport });
  const walletClient = createWalletClient({ account, transport });

  const chainId = await publicClient.getChainId();
  if (chainId !== network.chainId) {
    console.error(`Error: Expected chain ${network.chainId}, got ${chainId}`);
    process.exit(1);
  }

  const balance = await publicClient.getBalance({ address: account.address });
  if (balance < amountWei) {
    console.error(
      `Error: Insufficient balance. Have ${formatEther(balance)} ${network.symbol}, need ${amountStr} ${network.symbol}`
    );
    process.exit(1);
  }

  const gasReserve = parseEther("0.01");
  if (balance < amountWei + gasReserve) {
    console.error(
      `Warning: Balance may be too low for gas. Sending anyway. Consider leaving ~0.01 ${network.symbol} for fees.`
    );
  }

  console.log(`Sending ${amountStr} ${network.symbol} on ${network.name} from ${account.address} to ${toAddress}...`);

  const hash = await walletClient.sendTransaction({
    to: toAddress as `0x${string}`,
    value: amountWei,
    chain: {
      id: network.chainId,
      name: network.name,
      nativeCurrency: { decimals: 18, name: "COEN", symbol: network.symbol },
      rpcUrls: { default: { http: [network.rpc] } },
    },
  });

  console.log(`Tx hash: ${hash}`);
  console.log(`Waiting for confirmation...`);

  const receipt = await publicClient.waitForTransactionReceipt({ hash });
  if (receipt.status === "success") {
    console.log(`Done. Block: ${receipt.blockNumber}`);
  } else {
    console.error("Transaction failed.");
    process.exit(1);
  }
}

main();
