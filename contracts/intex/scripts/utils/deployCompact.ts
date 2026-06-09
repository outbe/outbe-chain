// Deploy The Compact V1 via Keyless CREATE2 Factory

import { network } from "hardhat";
import { type Hex, getAddress, type PublicClient, type WalletClient } from "viem";

interface L1NetworkConnection {
  viem: {
    getPublicClient(): Promise<PublicClient>;
    getWalletClients(): Promise<WalletClient[]>;
  };
  networkName: string;
}

// Keyless CREATE2 Factory (deployed on many chains at the same address)
const FACTORY_ADDRESS = "0x0000000000FFe8B47B3e2130213B802212439497" as const;

// Expected address of The Compact after deployment
const COMPACT_ADDRESS = "0x00000000000000171ede64904551eeDF3C6C9788" as const;

// Method selector for safeCreate2(bytes32,bytes)
const SAFE_CREATE2_SELECTOR = "0x64e03087" as const;

function assertHexPrefixed(value: string | undefined, name: string): asserts value is Hex {
  if (!value || typeof value !== "string" || !value.startsWith("0x")) {
    throw new Error(`${name} must be a hex string starting with 0x`);
  }
}

function validateFactoryData(data: Hex): void {
  if (!data.startsWith(SAFE_CREATE2_SELECTOR)) {
    throw new Error(
      `COMPACT_FACTORY_DATA does not start with ${SAFE_CREATE2_SELECTOR} (safeCreate2 selector). ` +
        "You likely did not copy the correct input data from the Ethereum deployment tx.\n" +
        `Received: ${data.slice(0, 10)}...`
    );
  }
}

async function main(): Promise<void> {
  const data = process.env.COMPACT_FACTORY_DATA;

  if (!data) {
    throw new Error(
      "Missing env var COMPACT_FACTORY_DATA.\n\n" +
        "To get this value:\n" +
        "1. Find The Compact deployment transaction on Ethereum mainnet\n" +
        "2. Copy the full 'Input Data' hex value\n" +
        "3. Add to .env: COMPACT_FACTORY_DATA=0x64e03087..."
    );
  }

  assertHexPrefixed(data, "COMPACT_FACTORY_DATA");
  validateFactoryData(data);

  // Get network connection (type assertion needed for L1 networks with viem support)
  const { viem, networkName } = await network.connect() as unknown as L1NetworkConnection;
  const publicClient = await viem.getPublicClient();
  const [walletClient] = await viem.getWalletClients();

  const account = walletClient.account;
  if (!account) {
    throw new Error("Wallet client has no account configured");
  }
  const signerAddress = account.address;
  const chainId = await publicClient.getChainId();

  console.log("=".repeat(60));
  console.log("Deploy The Compact via CREATE2 Factory");
  console.log("=".repeat(60));
  console.log("Network:", networkName);
  console.log("Chain ID:", chainId);
  console.log("Signer:", signerAddress);
  console.log("Factory:", FACTORY_ADDRESS);
  console.log("Expected Compact address:", COMPACT_ADDRESS);
  console.log("=".repeat(60));

  // 1) Verify factory exists on this network
  const factoryCode = await publicClient.getCode({ address: FACTORY_ADDRESS });
  if (!factoryCode || factoryCode === "0x") {
    throw new Error(
      `CREATE2 Factory has no code at ${FACTORY_ADDRESS} on ${networkName} (chainId: ${chainId}).\n` +
        "The Keyless CREATE2 Factory might not be deployed on this network."
    );
  }
  console.log("[OK] Factory code exists (size:", (factoryCode.length - 2) / 2, "bytes)");

  // 2) Check if The Compact is already deployed
  const compactCodeBefore = await publicClient.getCode({ address: COMPACT_ADDRESS });
  if (compactCodeBefore && compactCodeBefore !== "0x") {
    console.log("\n" + "=".repeat(60));
    console.log("[SKIP] The Compact is already deployed at", COMPACT_ADDRESS);
    console.log("Deployed code size:", (compactCodeBefore.length - 2) / 2, "bytes");
    console.log("=".repeat(60));
    return;
  }
  console.log("[OK] The Compact not yet deployed, proceeding...");

  // 3) Estimate gas (optional, helps catch issues early)
  console.log("\nEstimating gas...");
  let gasEstimate: bigint;
  try {
    gasEstimate = await publicClient.estimateGas({
      account: signerAddress,
      to: FACTORY_ADDRESS,
      data: data as Hex,
    });
    console.log("Estimated gas:", gasEstimate.toString());
  } catch (e) {
    console.warn("Gas estimation failed, using fallback (3,000,000):", e);
    gasEstimate = 3_000_000n;
  }

  // Add 20% buffer to gas estimate
  const gasLimit = (gasEstimate * 120n) / 100n;
  console.log("Gas limit (with 20% buffer):", gasLimit.toString());

  // 4) Send the transaction
  console.log("\nSending transaction to factory...");
  const txHash = await walletClient.sendTransaction({
    account,
    chain: walletClient.chain,
    to: FACTORY_ADDRESS,
    data: data as Hex,
    gas: gasLimit,
  });

  console.log("Transaction hash:", txHash);
  console.log("Waiting for confirmation...");

  const receipt = await publicClient.waitForTransactionReceipt({ hash: txHash });

  console.log("\n" + "-".repeat(60));
  console.log("Transaction mined!");
  console.log("Block number:", receipt.blockNumber);
  console.log("Gas used:", receipt.gasUsed.toString());
  console.log("Status:", receipt.status);
  console.log("-".repeat(60));

  if (receipt.status !== "success") {
    throw new Error(
      "Transaction was mined but status is not 'success'. " +
        "The factory call may have reverted internally."
    );
  }

  // 5) Verify deployment succeeded
  const compactCodeAfter = await publicClient.getCode({ address: COMPACT_ADDRESS });
  if (!compactCodeAfter || compactCodeAfter === "0x") {
    throw new Error(
      `Transaction succeeded but The Compact code is still empty at ${COMPACT_ADDRESS}.\n` +
        "This usually means:\n" +
        "- The calldata was incorrect\n" +
        "- The salt doesn't produce the expected address on this chain\n" +
        "- The factory call reverted internally"
    );
  }

  console.log("\n" + "=".repeat(60));
  console.log("SUCCESS! The Compact deployed at:", COMPACT_ADDRESS);
  console.log("Deployed code size:", (compactCodeAfter.length - 2) / 2, "bytes");
  console.log("=".repeat(60));

  // Verify address matches expected (sanity check)
  const normalizedExpected = getAddress(COMPACT_ADDRESS);
  console.log("\nVerification:");
  console.log("Expected address:", normalizedExpected);
  console.log("Contract has code: YES");
}

main().catch((error) => {
  console.error("\n[ERROR]", error.message || error);
  process.exit(1);
});
