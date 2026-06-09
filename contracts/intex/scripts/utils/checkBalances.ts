import { createPublicClient, http, formatEther } from "viem";
import { readFileSync } from "fs";
import { resolve } from "path";

const CHAINS = [
  { name: "Outbe Privnet", chainId: 512512, rpc: "https://eth.p.outbe.net", lzEid: 40512, symbol: "COEN" },
  { name: "Outbe Devnet", chainId: 424242, rpc: "https://eth.d.outbe.net/", lzEid: 40712, symbol: "COEN" },
  { name: "Outbe Testnet", chainId: 512215, rpc: "https://eth.testnet.outbe.net", lzEid: 40812, symbol: "COEN" },
  { name: "BSC Testnet", chainId: 97, rpc: "https://data-seed-prebsc-2-s1.bnbchain.org:8545", lzEid: 40102, symbol: "tBNB" },
  { name: "Base Sepolia", chainId: 84532, rpc: "https://sepolia.base.org", lzEid: 40245, symbol: "ETH" },
] as const;

interface Account {
  index: number;
  type: "value" | "user";
  outbe: string;
  eth: `0x${string}`;
  coen: number;
}

function parseAddressesFile(filePath: string): Account[] {
  const content = readFileSync(filePath, "utf-8");
  const accounts: Account[] = [];

  let currentType: "value" | "user" | null = null;

  for (const line of content.split("\n")) {
    if (line.includes("Value Accounts")) {
      currentType = "value";
      continue;
    }
    if (line.includes("User Accounts")) {
      currentType = "user";
      continue;
    }
    if (line.includes("Tributes")) {
      break; // stop before tributes section
    }

    if (!currentType) continue;

    // Match table rows: | # | outbe addr | eth addr | private key | COEN balance | ...
    const match = line.match(
      /\|\s*(\d+)\s*\|\s*`(outbe1\w+)`\s*\|\s*`(0x[0-9a-fA-F]+)`\s*\|\s*`0x[0-9a-fA-F]+`\s*\|\s*([\d,]+)/
    );
    if (!match) continue;

    accounts.push({
      index: parseInt(match[1]),
      type: currentType,
      outbe: match[2],
      eth: match[3] as `0x${string}`,
      coen: parseInt(match[4].replace(/,/g, "")),
    });
  }

  return accounts;
}

/** Check balance of a single address across all configured chains */
async function checkSingleAddress(address: `0x${string}`) {
  console.log(`Address: ${address}\n`);

  for (const chain of CHAINS) {
    let client: ReturnType<typeof createPublicClient>;
    try {
      client = createPublicClient({ transport: http(chain.rpc, { timeout: 10_000 }) });
      await client.getChainId(); // connectivity check
    } catch {
      console.log(`${chain.name} (${chain.chainId}) — UNREACHABLE\n`);
      continue;
    }

    const balance = await client.getBalance({ address });
    const status = balance === 0n ? "EMPTY" : "FUNDED";
    console.log(`${chain.name} (chain ${chain.chainId}): ${formatEther(balance)} ${chain.symbol}  ${status}`);
  }
}

async function main() {
  const arg1 = process.argv[2];
  const filePath = arg1?.startsWith("0x") ? undefined : (arg1 || resolve(import.meta.dirname!, "..", "addresses_v7.md"));

  // Quick mode: single address check
  if (arg1?.startsWith("0x") && arg1.length === 42) {
    await checkSingleAddress(arg1 as `0x${string}`);
    return;
  }

  let accounts: Account[];
  try {
    accounts = parseAddressesFile(filePath!);
  } catch (e) {
    console.error(`Failed to read ${filePath}: ${e instanceof Error ? e.message : e}`);
    process.exit(1);
  }

  console.log(`Parsed ${accounts.length} accounts from ${filePath}\n`);

  for (const chain of CHAINS) {
    let client: ReturnType<typeof createPublicClient>;
    try {
      client = createPublicClient({ transport: http(chain.rpc, { timeout: 10_000 }) });
      await client.getChainId(); // connectivity check
    } catch {
      console.log(`${chain.name} (${chain.chainId}) — UNREACHABLE\n`);
      continue;
    }

    console.log(`=== ${chain.name} (chain ${chain.chainId}, EID ${chain.lzEid}) ===\n`);

    let funded = 0;
    let totalBalance = 0n;

    // Batch balance queries in parallel (chunks of 10)
    for (let i = 0; i < accounts.length; i += 10) {
      const chunk = accounts.slice(i, i + 10);
      const balances = await Promise.all(
        chunk.map((a) => client.getBalance({ address: a.eth }).catch(() => 0n))
      );

      for (let j = 0; j < chunk.length; j++) {
        const acc = chunk[j];
        const bal = balances[j];
        const label = `${acc.type === "value" ? "Value" : "User "} #${String(acc.index).padStart(2)}`;
        const expectedUnit = BigInt(acc.coen) * 10n ** 18n;
        const balFormatted = formatEther(bal);

        if (bal === 0n) {
          console.log(`  ${label}  ${acc.eth}  ${balFormatted} ${chain.symbol}  EMPTY`);
        } else {
          funded++;
          totalBalance += bal;
          console.log(`  ${label}  ${acc.eth}  ${balFormatted} ${chain.symbol}  FUNDED`);
        }
      }
    }

    console.log(
      `\n  Summary: ${funded}/${accounts.length} funded, total ${formatEther(totalBalance)} ${chain.symbol}\n`
    );
  }
}

main();
