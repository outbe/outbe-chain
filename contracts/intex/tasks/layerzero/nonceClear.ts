import { task } from "hardhat/config";
import { createPublicClient, createWalletClient, http } from "viem";
import { privateKeyToAccount } from "viem/accounts";

import {
  ENDPOINT_NONCE_ABI,
  getEnvRpcAndPk,
  LZ_INFRA,
  makeChain,
  NETWORK_TO_EID,
  parsePacketV1,
  type PacketV1,
} from "../../scripts/shared/layerzero.js";

// ============================================================================
// ABIs (local — not in shared)
// ============================================================================

const ENDPOINT_SKIP_ABI = [
  {
    inputs: [
      { name: "_oapp", type: "address" },
      { name: "_srcEid", type: "uint32" },
      { name: "_sender", type: "bytes32" },
      { name: "_nonce", type: "uint64" },
    ],
    name: "skip",
    outputs: [],
    stateMutability: "nonpayable",
    type: "function",
  },
] as const;

const ENDPOINT_CLEAR_ABI = [
  {
    inputs: [
      { name: "_oapp", type: "address" },
      {
        name: "_origin",
        type: "tuple",
        components: [
          { name: "srcEid", type: "uint32" },
          { name: "sender", type: "bytes32" },
          { name: "nonce", type: "uint64" },
        ],
      },
      { name: "_guid", type: "bytes32" },
      { name: "_message", type: "bytes" },
    ],
    name: "clear",
    outputs: [],
    stateMutability: "nonpayable",
    type: "function",
  },
] as const;

// ============================================================================
// Paginated getLogs (RPC limit: 2000 blocks per request)
// ============================================================================

const MAX_BLOCK_RANGE = 2000n;

async function getPacketSentLogsPaginated(
  client: ReturnType<typeof createPublicClient>,
  fromBlock: bigint,
  toBlock: bigint,
): Promise<{ data: string; blockNumber: bigint }[]> {
  const allLogs: { data: string; blockNumber: bigint }[] = [];
  let cursor = fromBlock;

  while (cursor <= toBlock) {
    const end = cursor + MAX_BLOCK_RANGE - 1n > toBlock ? toBlock : cursor + MAX_BLOCK_RANGE - 1n;
    const logs = await client.getLogs({
      address: LZ_INFRA.endpoint,
      events: [{
        type: "event",
        name: "PacketSent",
        inputs: [
          { name: "encodedPayload", type: "bytes", indexed: false },
          { name: "options", type: "bytes", indexed: false },
          { name: "sendLibrary", type: "address", indexed: false },
        ],
      }],
      fromBlock: cursor,
      toBlock: end,
    });
    for (const l of logs) {
      allLogs.push({ data: l.data, blockNumber: l.blockNumber });
    }
    cursor = end + 1n;
  }

  return allLogs;
}

// ============================================================================
// Task action
// ============================================================================

interface ClearStuckNoncesArgs {
  oapp: string;
  srcOapp: string;
  srcNetwork: string;
  dstNetwork: string;
}

function withTypedArgs<TArgs>(
  fn: (args: TArgs) => Promise<void>,
): () => Promise<{ default: (args: unknown) => Promise<void> }> {
  return () => Promise.resolve({ default: (args) => fn(args as TArgs) });
}

const ZERO_HASH = "0x0000000000000000000000000000000000000000000000000000000000000000";

const clearStuckNoncesAction = async (args: ClearStuckNoncesArgs) => {
  const { oapp, srcOapp, srcNetwork, dstNetwork } = args;
  if (!oapp) throw new Error("--oapp (receiver OApp on dst) is required");
  if (!srcOapp) throw new Error("--src-oapp (sender OApp on src) is required");
  if (!srcNetwork) throw new Error("--src-network is required");
  if (!dstNetwork) throw new Error("--dst-network is required");

  const src = getEnvRpcAndPk(srcNetwork);
  const dst = getEnvRpcAndPk(dstNetwork);
  if (!dst.pk) throw new Error(`Private key not set for ${dstNetwork}`);

  const srcChain = makeChain(srcNetwork, src.rpc);
  const dstChain = makeChain(dstNetwork, dst.rpc);

  const srcPublic = createPublicClient({ chain: srcChain, transport: http(src.rpc) });
  const dstPublic = createPublicClient({ chain: dstChain, transport: http(dst.rpc) });

  const dstAccount = privateKeyToAccount(dst.pk as `0x${string}`);
  const dstWallet = createWalletClient({ account: dstAccount, chain: dstChain, transport: http(dst.rpc) });

  const oappAddr = oapp as `0x${string}`;
  const srcOappAddr = srcOapp as `0x${string}`;
  const srcEid = NETWORK_TO_EID[srcNetwork];
  const dstEid = NETWORK_TO_EID[dstNetwork];
  if (!srcEid || !dstEid) throw new Error(`Unknown EID for ${srcNetwork} or ${dstNetwork}`);

  const senderBytes32 = ("0x" + srcOappAddr.slice(2).toLowerCase().padStart(64, "0")) as `0x${string}`;
  const receiverBytes32 = ("0x" + oappAddr.slice(2).toLowerCase().padStart(64, "0")) as `0x${string}`;

  // 1. Read nonces
  const outbound = await srcPublic.readContract({
    address: LZ_INFRA.endpoint,
    abi: ENDPOINT_NONCE_ABI,
    functionName: "outboundNonce",
    args: [srcOappAddr, dstEid, receiverBytes32],
  });

  const lazyInbound = await dstPublic.readContract({
    address: LZ_INFRA.endpoint,
    abi: ENDPOINT_NONCE_ABI,
    functionName: "lazyInboundNonce",
    args: [oappAddr, srcEid, senderBytes32],
  });

  console.log(`Nonce state: outbound=${outbound}, lazyInbound=${lazyInbound}`);

  if (lazyInbound >= outbound) {
    console.log("✅ No stuck nonces — all delivered.");
    return;
  }

  const stuckCount = Number(outbound - lazyInbound);
  console.log(`${stuckCount} stuck nonce(s): ${lazyInbound + 1n}..${outbound}\n`);

  // 2. Classify each stuck nonce
  const stuckNonces: { nonce: bigint; verified: boolean; payloadHash: string }[] = [];
  for (let n = lazyInbound + 1n; n <= outbound; n++) {
    const hash = await dstPublic.readContract({
      address: LZ_INFRA.endpoint,
      abi: ENDPOINT_NONCE_ABI,
      functionName: "inboundPayloadHash",
      args: [oappAddr, srcEid, senderBytes32, n],
    });
    const verified = hash !== ZERO_HASH;
    stuckNonces.push({ nonce: n, verified, payloadHash: hash as string });
    console.log(`  nonce ${n}: ${verified ? "VERIFIED" : "UNVERIFIED"} (${hash})`);
  }

  // 3. For verified nonces, scan source chain for PacketSent events (paginated)
  const verifiedNonces = stuckNonces.filter((s) => s.verified);
  const packetsByNonce = new Map<bigint, PacketV1>();

  if (verifiedNonces.length > 0) {
    console.log(`\nScanning ${srcNetwork} for PacketSent events (paginated, max ${MAX_BLOCK_RANGE} blocks/req)...`);
    const latestBlock = await srcPublic.getBlockNumber();
    const scanFrom = latestBlock > 50000n ? latestBlock - 50000n : 0n;

    const rawLogs = await getPacketSentLogsPaginated(srcPublic, scanFrom, latestBlock);
    console.log(`  Found ${rawLogs.length} PacketSent event(s) in blocks ${scanFrom}..${latestBlock}`);

    for (const log of rawLogs) {
      try {
        const data = log.data;
        const packetOffset = Number("0x" + data.slice(2, 66)) * 2 + 2;
        const packetLen = Number("0x" + data.slice(packetOffset, packetOffset + 64));
        const packetHex = ("0x" + data.slice(packetOffset + 64, packetOffset + 64 + packetLen * 2)) as `0x${string}`;
        const pkt = parsePacketV1(packetHex);

        const pktSenderAddr = "0x" + pkt.sender.slice(-40).toLowerCase();
        if (pktSenderAddr === srcOappAddr.toLowerCase() && pkt.dstEid === dstEid) {
          packetsByNonce.set(pkt.nonce, pkt);
        }
      } catch {
        // skip unparsable
      }
    }
    console.log(`  Matched ${packetsByNonce.size} packet(s) for ${srcOappAddr} → EID ${dstEid}`);
  }

  // 4. Process stuck nonces sequentially
  console.log("\nClearing stuck nonces...");
  let cleared = 0;
  let skipped = 0;
  let failed = 0;

  for (const { nonce, verified } of stuckNonces) {
    if (!verified) {
      console.log(`  nonce ${nonce}: calling skip()...`);
      try {
        const tx = await dstWallet.writeContract({
          address: LZ_INFRA.endpoint,
          abi: ENDPOINT_SKIP_ABI,
          functionName: "skip",
          args: [oappAddr, srcEid, senderBytes32, nonce],
        });
        await dstPublic.waitForTransactionReceipt({ hash: tx });
        console.log(`    ✅ skipped (${tx})`);
        skipped++;
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        console.error(`    ❌ skip failed: ${msg.slice(0, 200)}`);
        failed++;
        break;
      }
    } else {
      const pkt = packetsByNonce.get(nonce);
      if (!pkt) {
        console.error(`    ❌ nonce ${nonce}: verified but PacketSent not found — cannot clear`);
        failed++;
        break;
      }
      console.log(`  nonce ${nonce}: calling clear() (guid=${pkt.guid.slice(0, 18)}...)...`);
      try {
        const tx = await dstWallet.writeContract({
          address: LZ_INFRA.endpoint,
          abi: ENDPOINT_CLEAR_ABI,
          functionName: "clear",
          args: [oappAddr, { srcEid, sender: senderBytes32, nonce }, pkt.guid, pkt.message],
        });
        await dstPublic.waitForTransactionReceipt({ hash: tx });
        console.log(`    ✅ cleared (${tx})`);
        cleared++;
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        console.error(`    ❌ clear failed: ${msg.slice(0, 200)}`);
        failed++;
        break;
      }
    }
  }

  // 5. Verify final state
  const finalLazy = await dstPublic.readContract({
    address: LZ_INFRA.endpoint,
    abi: ENDPOINT_NONCE_ABI,
    functionName: "lazyInboundNonce",
    args: [oappAddr, srcEid, senderBytes32],
  });

  console.log(`\nDone: skipped=${skipped}, cleared=${cleared}, failed=${failed}`);
  console.log(`Final lazyInboundNonce: ${finalLazy} (outboundNonce: ${outbound})`);

  if (finalLazy >= outbound) {
    console.log("✅ All nonces cleared — ready for fresh flow.");
  } else {
    console.warn(`⚠️  ${outbound - finalLazy} nonce(s) still pending.`);
  }
};

// ============================================================================
// Task definition
// ============================================================================

const clearStuckNonces = task("lz:clear-stuck-nonces", "Skip unverified + clear verified stuck LZ nonces so the channel is unblocked")
  .addOption({ name: "oapp", description: "Receiver OApp address on destination chain", defaultValue: "" })
  .addOption({ name: "srcOapp", description: "Sender OApp address on source chain", defaultValue: "" })
  .addOption({ name: "srcNetwork", description: "Source network name (outbeDevnet, bscTestnet, ...)", defaultValue: "" })
  .addOption({ name: "dstNetwork", description: "Destination network name (bscTestnet, outbeDevnet, ...)", defaultValue: "" })
  .setAction(withTypedArgs<ClearStuckNoncesArgs>(clearStuckNoncesAction));

// ============================================================================
// Export
// ============================================================================

export const lzNonceClearTasks = [clearStuckNonces.build()];
