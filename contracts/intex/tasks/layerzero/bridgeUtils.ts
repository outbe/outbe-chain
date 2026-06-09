import { task } from "hardhat/config";
import { ExecutorOptionType, Options } from "@layerzerolabs/lz-v2-utilities";
import { createPublicClient, createWalletClient, defineChain, encodeAbiParameters, getContract, http } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import {
  LZ_INFRA,
  NETWORK_CHAIN_IDS,
  NETWORK_TO_EID,
  OUTBE_CHAINS,
  PACKET_SENT_TOPIC,
  type PacketV1,
  getEnvRpcAndPk,
  makePublicClient,
  parsePacketV1,
} from "../../scripts/shared/layerzero.js";

const ENDPOINT_ABI = [
  {
    inputs: [
      { name: "_oapp", type: "address" },
      { name: "_lib", type: "address" },
      {
        name: "_params",
        type: "tuple[]",
        components: [
          { name: "eid", type: "uint32" },
          { name: "configType", type: "uint32" },
          { name: "config", type: "bytes" },
        ],
      },
    ],
    name: "setConfig",
    outputs: [],
    stateMutability: "nonpayable",
    type: "function",
  },
] as const;

// OAppCore.setDelegate — registers caller as Endpoint delegate (required for Endpoint.setConfig)
const OAPP_SET_DELEGATE_ABI = [
  {
    inputs: [{ name: "_delegate", type: "address" }],
    name: "setDelegate",
    outputs: [],
    stateMutability: "nonpayable",
    type: "function",
  },
] as const;

const OAPP_READ_ABI = [
  { inputs: [], name: "endpoint", outputs: [{ type: "address" }], stateMutability: "view", type: "function" },
] as const;

const ENDPOINT_READ_ABI = [
  { inputs: [{ name: "_oapp", type: "address" }], name: "delegates", outputs: [{ type: "address" }], stateMutability: "view", type: "function" },
  { inputs: [{ name: "_lib", type: "address" }], name: "isRegisteredLibrary", outputs: [{ type: "bool" }], stateMutability: "view", type: "function" },
] as const;

/** Hardhat 3: NetworkManager has no .name; get it from the connection or NETWORK env var. */
async function resolveNetworkName(hre: unknown): Promise<string> {
  const fromEnv = process.env.NETWORK;
  if (fromEnv) return fromEnv;
  const conn = await (hre as Hre).network.connect();
  return conn.networkName;
}

/** Outbe: viem + defineChain (hardhat-viem does not know custom chain IDs). Others: network.connect() */
async function getViemForLz(hre: unknown): Promise<{ getContractAt: (name: string, address: `0x${string}`) => Promise<unknown> }> {
  const networkName = await resolveNetworkName(hre);
  if (!(networkName in OUTBE_CHAINS)) {
    const { viem } = await (hre as Hre).network.connect();
    return viem;
  }
  const chain = OUTBE_CHAINS[networkName as keyof typeof OUTBE_CHAINS];
  const rpc = process.env.OUTBE_RPC_URL ?? chain.rpcUrls.default.http[0];
  const pk = process.env.OUTBE_PRIVATE_KEY;
  if (!pk) throw new Error("OUTBE_PRIVATE_KEY required for Outbe networks");
  const account = privateKeyToAccount(pk as `0x${string}`);
  const transport = http(rpc);
  const publicClient = createPublicClient({ chain, transport });
  const walletClient = createWalletClient({ account, chain, transport });
  const artifacts = (hre as { artifacts: { readArtifact: (name: string) => Promise<{ abi: unknown[] }> } }).artifacts;
  return {
    getContractAt: async (name: string, address: `0x${string}`) => {
      const { abi } = await artifacts.readArtifact(name);
      return getContract({ address, abi, client: { public: publicClient, wallet: walletClient } });
    },
  };
}

/** (eid, msgType, options) for setEnforcedOptions. */
type EnforcedOptionParam = { eid: number; msgType: number; options: `0x${string}` };

function getPeerEids(pathways: Array<[{ eid: number }, { eid: number }, unknown?, unknown?, unknown?]>): (currentEid: number) => number[] {
  return (currentEid: number) => {
    const peers = new Set<number>();
    for (const [a, b] of pathways) {
      if (a.eid === currentEid) peers.add(b.eid);
      if (b.eid === currentEid) peers.add(a.eid);
    }
    return [...peers];
  };
}

function encodeOption(opt: { optionType: number; gas: number; value?: number }): `0x${string}` {
  if (opt.optionType !== ExecutorOptionType.LZ_RECEIVE) {
    throw new Error(`Unsupported optionType: ${opt.optionType}`);
  }
  const hex = Options.newOptions()
    .addExecutorLzReceiveOption(opt.gas, opt.value ?? 0)
    .toHex()
    .toString();
  return (hex.startsWith("0x") ? hex : `0x${hex}`) as `0x${string}`;
}

interface SetEnforcedOptionsArgs {
  adapter: string;
  /** onft | batch | bridge — which layerzero config to use for pathways and options */
  lzConfig: string;
  /** Contract name (default from config: ONFT1155Adapter, ONFT1155AdapterBatch, or TargetMessenger/OriginMessenger by network) */
  contract?: string;
}

interface Hre {
  network: {
    connect: () => Promise<{
      networkName: string;
      viem: {
        getWalletClients: () => Promise<Array<{ account: { address: `0x${string}` } }>>;
        getContractAt: (name: string, address: `0x${string}`) => Promise<unknown>;
      };
    }>;
  };
}

interface GrantBridgeRoleArgs {
  token: string;
  adapter: string;
  /** Contract with AccessControl (Auction | EscrowAdapter | IntexNFT1155). Default: IntexNFT1155 */
  contract?: string;
}

interface SetPeerArgs {
  adapter: string;
  peerEid: string;
  peerAddress: string;
  /** Contract name: ONFT1155Adapter | ONFT1155AdapterBatch | TargetMessenger | OriginMessenger */
  contract: string;
}

interface CheckPeerArgs {
  adapter: string;
  peerEid: string;
  contract: string;
}

interface QuoteSendArgs {
  adapter: string;
  dstEid: string;
  tokenId: string;
  amount: string;
  to?: string;
}

/** Wraps a typed task action for Hardhat: single cast at call site, actions keep typed args. */
function withTypedArgs<TArgs>(
  fn: (args: TArgs, hre: unknown) => Promise<void>
): () => Promise<{ default: (args: unknown, hre: unknown) => Promise<void> }> {
  return () => Promise.resolve({
    default: (args, hre) => fn(args as TArgs, hre),
  });
}

// ============================================================================
// Grant RELAYER_ROLE
// ============================================================================

const grantBridgeRoleAction = async (args: GrantBridgeRoleArgs, hre: unknown) => {
  const viem = await getViemForLz(hre);
  const contractName = args.contract || "IntexNFT1155";
  console.log(`Granting RELAYER_ROLE...`);
  console.log(`  Contract: ${contractName} @ ${args.token}`);
  console.log(`  Grantee: ${args.adapter}`);
  const token = (await viem.getContractAt(
    contractName,
    args.token as `0x${string}`
  )) as {
    read: {
      RELAYER_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const RELAYER_ROLE = await token.read.RELAYER_ROLE();
  const hasRole = await token.read.hasRole([RELAYER_ROLE, args.adapter as `0x${string}`]);

  if (hasRole) {
    console.log("✅ RELAYER_ROLE already granted");
    return;
  }

  const txHash = await token.write.grantRole([RELAYER_ROLE, args.adapter as `0x${string}`]);
  const networkName = await resolveNetworkName(hre);
  await makePublicClient(networkName).waitForTransactionReceipt({ hash: txHash });
  console.log(`✅ RELAYER_ROLE granted. Tx: ${txHash}`);
};

const grantBridgeRole = task(
  "lz:grant-bridge-role",
  "Grant RELAYER_ROLE on contract (Auction, EscrowAdapter, IntexNFT1155) to adapter"
)
  .addOption({
    name: "token",
    description: "Contract address (Auction, EscrowAdapter, or IntexNFT1155)",
    defaultValue: "",
  })
  .addOption({
    name: "adapter",
    description: "Address to grant RELAYER_ROLE to (e.g. TargetMessenger, ONFT1155Adapter)",
    defaultValue: "",
  })
  .addOption({
    name: "contract",
    description: "Contract name: Auction | EscrowAdapter | IntexNFT1155 (default: IntexNFT1155)",
    defaultValue: "",
  })
  .setAction(withTypedArgs<GrantBridgeRoleArgs>(grantBridgeRoleAction));

// ============================================================================
// Grant SYSTEM_RELAYER_ROLE
// IntexNFT1155 gates `debit`/`credit` in the `Called` state behind this role.
// Used to whitelist the system batch adapter (ONFT1155AdapterBatch) which moves
// holder balances cross-chain after `markCalled`.
// ============================================================================

const grantSystemRelayerRoleAction = async (args: GrantBridgeRoleArgs, hre: unknown) => {
  const viem = await getViemForLz(hre);
  const contractName = args.contract || "IntexNFT1155";
  console.log(`Granting SYSTEM_RELAYER_ROLE...`);
  console.log(`  Contract: ${contractName} @ ${args.token}`);
  console.log(`  Grantee: ${args.adapter}`);
  const token = (await viem.getContractAt(
    contractName,
    args.token as `0x${string}`
  )) as {
    read: {
      SYSTEM_RELAYER_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const role = await token.read.SYSTEM_RELAYER_ROLE();
  const hasRole = await token.read.hasRole([role, args.adapter as `0x${string}`]);
  if (hasRole) {
    console.log("✅ SYSTEM_RELAYER_ROLE already granted");
    return;
  }

  const txHash = await token.write.grantRole([role, args.adapter as `0x${string}`]);
  const networkName = await resolveNetworkName(hre);
  await makePublicClient(networkName).waitForTransactionReceipt({ hash: txHash });
  console.log(`✅ SYSTEM_RELAYER_ROLE granted. Tx: ${txHash}`);
};

const grantSystemRelayerRole = task(
  "lz:grant-system-relayer-role",
  "Grant SYSTEM_RELAYER_ROLE on IntexNFT1155 to a system bridge adapter"
)
  .addOption({
    name: "token",
    description: "IntexNFT1155 contract address",
    defaultValue: "",
  })
  .addOption({
    name: "adapter",
    description: "Address to grant SYSTEM_RELAYER_ROLE to (e.g. ONFT1155AdapterBatch)",
    defaultValue: "",
  })
  .addOption({
    name: "contract",
    description: "Contract name (default: IntexNFT1155)",
    defaultValue: "",
  })
  .setAction(withTypedArgs<GrantBridgeRoleArgs>(grantSystemRelayerRoleAction));

// ============================================================================
// Set Peer
// ============================================================================

const setPeerAction = async (args: SetPeerArgs, hre: unknown) => {
  const viem = await getViemForLz(hre);
  const contractName = args.contract || "ONFT1155Adapter";
  const peerEid = parseInt(args.peerEid, 10);
  const peerBytes32 = `0x${args.peerAddress.slice(2).toLowerCase().padStart(64, "0")}` as `0x${string}`;

  console.log(`Setting peer...`);
  console.log(`  Adapter: ${args.adapter}`);
  console.log(`  Peer EID: ${peerEid}`);
  console.log(`  Peer Address: ${args.peerAddress}`);
  console.log(`  Peer bytes32: ${peerBytes32}`);

  const adapter = (await viem.getContractAt(
    contractName,
    args.adapter as `0x${string}`
  )) as {
    read: {
      peers: (args: [number]) => Promise<`0x${string}`>;
    };
    write: {
      setPeer: (args: [number, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  // Check current peer
  const currentPeer = await adapter.read.peers([peerEid]);
  if (currentPeer !== "0x0000000000000000000000000000000000000000000000000000000000000000") {
    console.log(`⚠️  Peer already set: ${currentPeer}`);
    if (currentPeer.toLowerCase() === peerBytes32.toLowerCase()) {
      console.log("✅ Peer is correctly configured");
      return;
    }
    console.log("Updating peer...");
  }

  const txHash = await adapter.write.setPeer([peerEid, peerBytes32]);
  const networkName = await resolveNetworkName(hre);
  await makePublicClient(networkName).waitForTransactionReceipt({ hash: txHash });
  console.log(`✅ Peer set. Tx: ${txHash}`);
};

const setPeer = task("lz:set-peer", "Set peer adapter for cross-chain communication (ONFT or Bridge adapters)")
  .addOption({
    name: "adapter",
    description: "Local adapter contract address",
    defaultValue: "",
  })
  .addOption({
    name: "peerEid",
    description: "Remote chain endpoint ID",
    defaultValue: "",
  })
  .addOption({
    name: "peerAddress",
    description: "Remote adapter address",
    defaultValue: "",
  })
  .addOption({
    name: "contract",
    description: "Adapter contract name: ONFT1155Adapter | ONFT1155AdapterBatch | TargetMessenger | OriginMessenger",
    defaultValue: "ONFT1155Adapter",
  })
  .setAction(withTypedArgs<SetPeerArgs>(setPeerAction));

// ============================================================================
// Set Enforced Options (prod toolbox-style config from layerzero.*.config.ts)
// ============================================================================

const setEnforcedOptionsAction = async (args: SetEnforcedOptionsArgs, hre: unknown) => {
  const viem = await getViemForLz(hre);

  const networkName = await resolveNetworkName(hre);
  const currentEid = networkName ? NETWORK_TO_EID[networkName] : undefined;
  if (currentEid == null) {
    throw new Error(
      `Unknown network for EID: ${networkName ?? "undefined"}. Known: ${Object.keys(NETWORK_TO_EID).join(", ")}. ` +
        `In CI, set NETWORK env (e.g. NETWORK=bscTestnet) if hardhat.network.name is not set.`
    );
  }

  type ConfigModule = {
    pathways: Array<[{ eid: number }, { eid: number }, unknown?, unknown?, unknown?]>;
    EVM_ENFORCED_OPTIONS?: Array<{ msgType: number; optionType: number; gas: number; value?: number }>;
    BRIDGE_ENFORCED_OPTIONS?: Array<{ msgType: number; optionType: number; gas: number; value?: number }>;
  };

  let mod: ConfigModule;
  let contractName: string;

  if (args.lzConfig === "onft") {
    // @ts-expect-error - dynamic path from repo root
    mod = (await import("../../config/layerzero.config")) as unknown as ConfigModule;
    contractName = args.contract || "ONFT1155Adapter";
  } else if (args.lzConfig === "batch") {
    // @ts-expect-error - dynamic path from repo root
    mod = (await import("../../config/layerzero.batch.config")) as unknown as ConfigModule;
    contractName = args.contract || "ONFT1155AdapterBatch";
  } else if (args.lzConfig === "bridge") {
    // @ts-expect-error - dynamic path from repo root
    mod = (await import("../../config/layerzero.bridge.config")) as unknown as ConfigModule;
    // BSC (testnet 40102, mainnet 30102) uses TargetMessenger, Outbe uses OriginMessenger
    const isBsc = currentEid === 40102 || currentEid === 30102;
    contractName = args.contract || (isBsc ? "TargetMessenger" : "OriginMessenger");
  } else {
    throw new Error(`--lz-config must be onft | batch | bridge, got: ${args.lzConfig}`);
  }

  const options = mod.EVM_ENFORCED_OPTIONS ?? mod.BRIDGE_ENFORCED_OPTIONS ?? [];
  const getPeers = getPeerEids(mod.pathways);
  const peerEids = getPeers(currentEid);

  const params: EnforcedOptionParam[] = [];
  for (const peerEid of peerEids) {
    for (const opt of options) {
      params.push({
        eid: peerEid,
        msgType: opt.msgType,
        options: encodeOption(opt),
      });
    }
  }

  if (params.length === 0) {
    console.log(`No enforced options to set (peer eids: ${peerEids.join(", ")}, options: ${options.length})`);
    return;
  }

  console.log(`Setting enforced options on ${networkName}...`);
  console.log(`  Adapter: ${args.adapter}`);
  console.log(`  Contract: ${contractName}`);
  console.log(`  Params: ${params.length} (${peerEids.length} peer(s) × ${options.length} msgTypes)`);

  const adapter = (await viem.getContractAt(
    contractName,
    args.adapter as `0x${string}`
  )) as {
    write: {
      setEnforcedOptions: (args: [EnforcedOptionParam[]]) => Promise<`0x${string}`>;
    };
  };

  const txHash = await adapter.write.setEnforcedOptions([params]);
  await makePublicClient(networkName).waitForTransactionReceipt({ hash: txHash });
  console.log(`✅ setEnforcedOptions. Tx: ${txHash}`);
};

const setEnforcedOptions = task(
  "lz:set-enforced-options",
  "Set enforced options on adapter from layerzero config (onft / batch / bridge)"
)
  .addOption({
    name: "adapter",
    description: "Adapter contract address",
    defaultValue: "",
  })
  .addOption({
    name: "lzConfig",
    description: "LayerZero config to use: onft | batch | bridge",
    defaultValue: "onft",
  })
  .addOption({
    name: "contract",
    description: "Contract name (default: ONFT1155Adapter / ONFT1155AdapterBatch / TargetMessenger|OriginMessenger by network)",
    defaultValue: "",
  })
  .setAction(withTypedArgs<SetEnforcedOptionsArgs>(setEnforcedOptionsAction));

// ============================================================================
// Check Peer
// ============================================================================
const checkPeerAction = async (args: CheckPeerArgs, hre: unknown) => {
  const viem = await getViemForLz(hre);
  
  const peerEid = parseInt(args.peerEid, 10);

  console.log(`Checking peer...`);
  console.log(`  Adapter: ${args.adapter}`);
  console.log(`  Peer EID: ${peerEid}`);

  const contractName = args.contract || "ONFT1155Adapter";
  const adapter = (await viem.getContractAt(
    contractName,
    args.adapter as `0x${string}`
  )) as {
    read: {
      peers: (args: [number]) => Promise<`0x${string}`>;
    };
  };

  const peer = await adapter.read.peers([peerEid]);

  if (peer === "0x0000000000000000000000000000000000000000000000000000000000000000") {
    console.log("❌ No peer set for this EID");
  } else {
    // Convert bytes32 to address
    const peerAddress = "0x" + peer.slice(-40);
    console.log(`✅ Peer: ${peerAddress}`);
    console.log(`   Raw: ${peer}`);
  }
};

const checkPeer = task("lz:check-peer", "Check peer configuration for an adapter")
  .addOption({
    name: "adapter",
    description: "Adapter contract address",
    defaultValue: "",
  })
  .addOption({
    name: "peerEid",
    description: "Remote chain endpoint ID",
    defaultValue: "",
  })
  .addOption({
    name: "contract",
    description: "Adapter contract name: ONFT1155Adapter | ONFT1155AdapterBatch | TargetMessenger | OriginMessenger",
    defaultValue: "ONFT1155Adapter",
  })
  .setAction(withTypedArgs<CheckPeerArgs>(checkPeerAction));

// ============================================================================
// Quote Send
// ============================================================================

const quoteSendAction = async (args: QuoteSendArgs, hre: unknown) => {
  const { viem } = await (hre as Hre).network.connect();
  
  const dstEid = parseInt(args.dstEid, 10);
  const tokenId = BigInt(args.tokenId);
  const amount = BigInt(args.amount);

  console.log(`Quoting send cost...`);

  const [walletClient] = await viem.getWalletClients();
  const recipient = (args.to || walletClient.account.address) as `0x${string}`;
  const toBytes32 = `0x${recipient.slice(2).padStart(64, "0")}` as `0x${string}`;

  const adapter = (await viem.getContractAt(
    "ONFT1155Adapter",
    args.adapter as `0x${string}`
  )) as {
    read: {
      quoteSend: (args: [unknown, boolean]) => Promise<{ nativeFee: bigint; lzTokenFee: bigint }>;
    };
  };

  const extraOptions = Options.newOptions()
    .addExecutorLzReceiveOption(200000, 0)
    .toHex()
    .toString();

  const sendParam = {
    dstEid,
    to: toBytes32,
    tokenId,
    amount,
    extraOptions,
    composeMsg: "0x" as `0x${string}`,
  };

  const fee = await adapter.read.quoteSend([sendParam, false]);
  const nativeFeeEth = Number(fee.nativeFee) / 1e18;

  console.log(`\n📊 Quote Results:`);
  console.log(`   Destination EID: ${dstEid}`);
  console.log(`   Token ID: ${tokenId}`);
  console.log(`   Amount: ${amount}`);
  console.log(`   Recipient: ${recipient}`);
  console.log(`\n💰 Fee:`);
  console.log(`   Native: ${nativeFeeEth.toFixed(6)} ETH/BNB`);
  console.log(`   LZ Token: ${fee.lzTokenFee.toString()}`);
  console.log(`   Wei: ${fee.nativeFee.toString()}`);
};

const quoteSend = task("lz:quote-send", "Estimate cross-chain transfer cost")
  .addOption({
    name: "adapter",
    description: "ONFT1155Adapter address",
    defaultValue: "",
  })
  .addOption({
    name: "dstEid",
    description: "Destination endpoint ID",
    defaultValue: "",
  })
  .addOption({
    name: "tokenId",
    description: "Token ID",
    defaultValue: "",
  })
  .addOption({
    name: "amount",
    description: "Amount to transfer",
    defaultValue: "",
  })
  .addOption({
    name: "to",
    description: "Recipient (defaults to sender)",
    defaultValue: "",
  })
  .setAction(withTypedArgs<QuoteSendArgs>(quoteSendAction));

// ============================================================================
// Set ULN Config (DVN + Executor on SendUln302/ReceiveUln302)
// ============================================================================

interface SetUlnConfigArgs {
  adapter: string;
  remoteEid: string;
}

const setUlnConfigAction = async (args: SetUlnConfigArgs, hre: unknown) => {
  const networkName = await resolveNetworkName(hre);
  const remoteEid = parseInt(args.remoteEid, 10);
  const oapp = args.adapter as `0x${string}`;

  const { rpc, pk } = getEnvRpcAndPk(networkName);
  if (!pk) throw new Error(`Private key not set for ${networkName}`);
  const chainId = NETWORK_CHAIN_IDS[networkName];
  if (!chainId) throw new Error(`Unknown chain ID for ${networkName}`);

  const account = privateKeyToAccount(pk as `0x${string}`);
  const chain =
    networkName in OUTBE_CHAINS
      ? OUTBE_CHAINS[networkName as keyof typeof OUTBE_CHAINS]
      : defineChain({
          id: chainId,
          name: networkName,
          nativeCurrency: { decimals: 18, name: "BNB", symbol: "BNB" },
          rpcUrls: { default: { http: [rpc] } },
        });

  const transport = http(rpc);
  const walletClient = createWalletClient({ account, chain, transport });
  const publicClient = createPublicClient({ chain, transport });

  console.log(`Setting ULN config on ${networkName}...`);
  console.log(`  OApp: ${oapp}, Remote EID: ${remoteEid}, Caller: ${account.address}`);

  // Guard: OApp must point to the custom Endpoint where our infrastructure is registered
  const oappEndpoint = await publicClient.readContract({ address: oapp, abi: OAPP_READ_ABI, functionName: "endpoint" }) as string;
  if (oappEndpoint.toLowerCase() !== LZ_INFRA.endpoint.toLowerCase()) {
    throw new Error(
      `OApp.endpoint() = ${oappEndpoint}, expected ${LZ_INFRA.endpoint}. ` +
      `Redeploy the OApp on ${networkName} with the custom Endpoint address.`,
    );
  }

  // Pre-flight: verify libraries have bytecode and are registered
  const [sendLibCode, recvLibCode, sendLibRegistered, recvLibRegistered] = await Promise.all([
    publicClient.getCode({ address: LZ_INFRA.sendUln302 }),
    publicClient.getCode({ address: LZ_INFRA.receiveUln302 }),
    publicClient.readContract({ address: LZ_INFRA.endpoint, abi: ENDPOINT_READ_ABI, functionName: "isRegisteredLibrary", args: [LZ_INFRA.sendUln302] }),
    publicClient.readContract({ address: LZ_INFRA.endpoint, abi: ENDPOINT_READ_ABI, functionName: "isRegisteredLibrary", args: [LZ_INFRA.receiveUln302] }),
  ]);
  if (!sendLibCode || sendLibCode === "0x") {
    throw new Error(`SendUln302 (${LZ_INFRA.sendUln302}) has no bytecode on ${networkName}. LZ infrastructure not deployed.`);
  }
  if (!recvLibCode || recvLibCode === "0x") {
    throw new Error(`ReceiveUln302 (${LZ_INFRA.receiveUln302}) has no bytecode on ${networkName}. LZ infrastructure not deployed.`);
  }
  if (!sendLibRegistered) {
    throw new Error(`SendUln302 (${LZ_INFRA.sendUln302}) is not registered on Endpoint on ${networkName}.`);
  }
  if (!recvLibRegistered) {
    throw new Error(`ReceiveUln302 (${LZ_INFRA.receiveUln302}) is not registered on Endpoint on ${networkName}.`);
  }

  // Endpoint.setConfig requires msg.sender == delegates[oapp]
  const delegate = await publicClient.readContract({
    address: LZ_INFRA.endpoint, abi: ENDPOINT_READ_ABI, functionName: "delegates", args: [oapp],
  }) as string;

  if (delegate.toLowerCase() !== account.address.toLowerCase()) {
    console.log(`  Setting deployer as Endpoint delegate...`);
    const tx = await walletClient.writeContract({
      address: oapp, abi: OAPP_SET_DELEGATE_ABI, functionName: "setDelegate", args: [account.address],
    });
    await publicClient.waitForTransactionReceipt({ hash: tx });
    console.log(`  setDelegate confirmed (${tx})`);
  }

  const executorConfig = encodeAbiParameters(
    [{ type: "uint32" }, { type: "address" }],
    [LZ_INFRA.maxMessageSize, LZ_INFRA.executor],
  );

  // UlnConfig is a dynamic tuple (contains address[]), so abi.decode(data, (UlnConfig))
  // expects a leading offset word. Encoding as a tuple type produces the correct layout;
  // encoding as flat params would omit the offset and cause an empty revert.
  const ulnConfig = encodeAbiParameters(
    [{
      type: "tuple",
      components: [
        { name: "confirmations", type: "uint64" },
        { name: "requiredDVNCount", type: "uint8" },
        { name: "optionalDVNCount", type: "uint8" },
        { name: "optionalDVNThreshold", type: "uint8" },
        { name: "requiredDVNs", type: "address[]" },
        { name: "optionalDVNs", type: "address[]" },
      ],
    }],
    [{
      confirmations: LZ_INFRA.confirmations,
      requiredDVNCount: 1,
      optionalDVNCount: 0,
      optionalDVNThreshold: 0,
      requiredDVNs: [LZ_INFRA.dvn],
      optionalDVNs: [],
    }],
  );

  // One SetConfigParam per call — batching multiple dynamic-bytes params
  // in a single array causes abi.decode failures in this custom SendUln302.
  console.log(`  Configuring SendUln302 — ExecutorConfig...`);
  const tx1 = await walletClient.writeContract({
    address: LZ_INFRA.endpoint,
    abi: ENDPOINT_ABI,
    functionName: "setConfig",
    args: [oapp, LZ_INFRA.sendUln302, [{ eid: remoteEid, configType: 1, config: executorConfig }]],
  });
  await publicClient.waitForTransactionReceipt({ hash: tx1 });

  console.log(`  Configuring SendUln302 — UlnConfig...`);
  const tx2 = await walletClient.writeContract({
    address: LZ_INFRA.endpoint,
    abi: ENDPOINT_ABI,
    functionName: "setConfig",
    args: [oapp, LZ_INFRA.sendUln302, [{ eid: remoteEid, configType: 2, config: ulnConfig }]],
  });
  await publicClient.waitForTransactionReceipt({ hash: tx2 });

  console.log(`  Configuring ReceiveUln302 — UlnConfig...`);
  const tx3 = await walletClient.writeContract({
    address: LZ_INFRA.endpoint,
    abi: ENDPOINT_ABI,
    functionName: "setConfig",
    args: [oapp, LZ_INFRA.receiveUln302, [{ eid: remoteEid, configType: 2, config: ulnConfig }]],
  });
  await publicClient.waitForTransactionReceipt({ hash: tx3 });

  console.log(`ULN config set for ${networkName} → EID ${remoteEid}`);
};

const setUlnConfig = task("lz:set-uln-config", "Configure SendUln302/ReceiveUln302 with DVN and Executor for an OApp")
  .addOption({
    name: "adapter",
    description: "OApp (adapter) contract address",
    defaultValue: "",
  })
  .addOption({
    name: "remoteEid",
    description: "Remote chain endpoint ID",
    defaultValue: "",
  })
  .setAction(withTypedArgs<SetUlnConfigArgs>(setUlnConfigAction));

// ============================================================================
// lz:manual-deliver — deliver a verified-but-unexecuted LZ message manually
// ============================================================================

const LZ_RECEIVE_ABI = [
  {
    inputs: [
      {
        name: "_origin",
        type: "tuple",
        components: [
          { name: "srcEid", type: "uint32" },
          { name: "sender", type: "bytes32" },
          { name: "nonce", type: "uint64" },
        ],
      },
      { name: "_receiver", type: "address" },
      { name: "_guid", type: "bytes32" },
      { name: "_message", type: "bytes" },
      { name: "_extraData", type: "bytes" },
    ],
    name: "lzReceive",
    outputs: [],
    stateMutability: "payable",
    type: "function",
  },
] as const;

interface ManualDeliverArgs {
  srcTxHash: string;
  srcNetwork: string;
  dstNetwork: string;
  gasLimit: string;
  value: string;
}

const manualDeliverAction = async (args: ManualDeliverArgs) => {
  const { srcTxHash, srcNetwork, dstNetwork, gasLimit } = args;
  if (!srcTxHash) throw new Error("--src-tx-hash is required");
  if (!srcNetwork) throw new Error("--src-network is required");
  if (!dstNetwork) throw new Error("--dst-network is required");

  // --- Source chain: fetch PacketSent event ---
  const src = getEnvRpcAndPk(srcNetwork);
  const srcChainId = NETWORK_CHAIN_IDS[srcNetwork];
  if (!srcChainId) throw new Error(`Unknown source network: ${srcNetwork}`);

  const srcChain =
    srcNetwork.startsWith("outbe") && srcNetwork in OUTBE_CHAINS
      ? OUTBE_CHAINS[srcNetwork as keyof typeof OUTBE_CHAINS]
      : defineChain({
          id: srcChainId,
          name: srcNetwork,
          nativeCurrency: { decimals: 18, name: "ETH", symbol: "ETH" },
          rpcUrls: { default: { http: [src.rpc] } },
        });

  const srcPublic = createPublicClient({ chain: srcChain, transport: http(src.rpc) });

  console.log(`Fetching tx receipt from ${srcNetwork}...`);
  const receipt = await srcPublic.getTransactionReceipt({ hash: srcTxHash as `0x${string}` });

  const packetLog = receipt.logs.find(
    (l) => l.topics[0]?.toLowerCase() === PACKET_SENT_TOPIC.toLowerCase(),
  );
  if (!packetLog) throw new Error("PacketSent event not found in tx receipt");

  // Decode: (bytes encodedPacket, bytes options, address sendLibrary)
  const data = packetLog.data;
  // First word = offset of encodedPacket, read length at that offset, then raw bytes
  const packetOffset = Number("0x" + data.slice(2, 66)) * 2 + 2; // byte offset → hex offset + 0x
  const packetLen = Number("0x" + data.slice(packetOffset, packetOffset + 64));
  const packetHex = ("0x" + data.slice(packetOffset + 64, packetOffset + 64 + packetLen * 2)) as `0x${string}`;

  const pkt = parsePacketV1(packetHex);
  console.log(`\nParsed PacketV1:`);
  console.log(`  nonce:    ${pkt.nonce}`);
  console.log(`  srcEid:   ${pkt.srcEid}`);
  console.log(`  sender:   ${pkt.sender}`);
  console.log(`  dstEid:   ${pkt.dstEid}`);
  console.log(`  receiver: ${pkt.receiver}`);
  console.log(`  guid:     ${pkt.guid}`);
  console.log(`  message:  ${pkt.message}`);

  // --- Destination chain: call lzReceive ---
  const dst = getEnvRpcAndPk(dstNetwork);
  if (!dst.pk) throw new Error(`Private key not set for ${dstNetwork}`);
  const dstChainId = NETWORK_CHAIN_IDS[dstNetwork];
  if (!dstChainId) throw new Error(`Unknown destination network: ${dstNetwork}`);

  const dstChain =
    dstNetwork.startsWith("outbe") && dstNetwork in OUTBE_CHAINS
      ? OUTBE_CHAINS[dstNetwork as keyof typeof OUTBE_CHAINS]
      : defineChain({
          id: dstChainId,
          name: dstNetwork,
          nativeCurrency: { decimals: 18, name: "ETH", symbol: "ETH" },
          rpcUrls: { default: { http: [dst.rpc] } },
        });

  const dstAccount = privateKeyToAccount(dst.pk as `0x${string}`);
  const dstPublic = createPublicClient({ chain: dstChain, transport: http(dst.rpc) });
  const dstWallet = createWalletClient({ account: dstAccount, chain: dstChain, transport: http(dst.rpc) });

  const receiverAddress = ("0x" + pkt.receiver.slice(-40)) as `0x${string}`;

  const sendValue = args.value ? BigInt(args.value) : 0n;

  console.log(`\nDelivering on ${dstNetwork} via Endpoint.lzReceive()...`);
  console.log(`  endpoint: ${LZ_INFRA.endpoint}`);
  console.log(`  receiver: ${receiverAddress}`);
  console.log(`  gasLimit: ${gasLimit}`);
  if (sendValue > 0n) console.log(`  value:    ${sendValue} wei`);

  const tx = await dstWallet.writeContract({
    address: LZ_INFRA.endpoint,
    abi: LZ_RECEIVE_ABI,
    functionName: "lzReceive",
    args: [
      { srcEid: pkt.srcEid, sender: pkt.sender, nonce: pkt.nonce },
      receiverAddress,
      pkt.guid,
      pkt.message,
      "0x",
    ],
    gas: BigInt(gasLimit),
    value: sendValue,
  });

  console.log(`  tx: ${tx}`);
  const txReceipt = await dstPublic.waitForTransactionReceipt({ hash: tx });
  console.log(`  status: ${txReceipt.status}`);
  console.log(`  gasUsed: ${txReceipt.gasUsed}`);

  if (txReceipt.status === "reverted") {
    console.error(`\n❌ lzReceive reverted! The payload hash was consumed but _lzReceive failed.`);
    console.error(`   Check the revert reason: cast run ${tx} --rpc-url <dst-rpc>`);
    process.exitCode = 1;
  } else {
    console.log(`\n✅ Message delivered successfully!`);
  }
};

const manualDeliver = task("lz:manual-deliver", "Manually deliver a verified-but-unexecuted LZ message by calling Endpoint.lzReceive()")
  .addOption({ name: "srcTxHash", description: "Tx hash on source chain that emitted PacketSent", defaultValue: "" })
  .addOption({ name: "srcNetwork", description: "Source network name (outbeDevnet, bscTestnet, ...)", defaultValue: "" })
  .addOption({ name: "dstNetwork", description: "Destination network name (bscTestnet, outbeDevnet, ...)", defaultValue: "" })
  .addOption({ name: "gasLimit", description: "Gas limit for lzReceive call", defaultValue: "2000000" })
  .addOption({ name: "value", description: "Native value (wei) to forward with lzReceive (for receivers that do outbound _lzSend)", defaultValue: "0" })
  .setAction(withTypedArgs<ManualDeliverArgs>(manualDeliverAction));

// ============================================================================
// Export
// ============================================================================

export const lzBridgeUtilTasks = [
  grantBridgeRole.build(),
  grantSystemRelayerRole.build(),
  setPeer.build(),
  setEnforcedOptions.build(),
  setUlnConfig.build(),
  checkPeer.build(),
  quoteSend.build(),
  manualDeliver.build(),
];
