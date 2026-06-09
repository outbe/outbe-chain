// LayerZero config for TargetMessenger (BSC) ↔ OriginMessenger (Outbe)
// Used by lz:set-enforced-options task
import { EndpointId } from "@layerzerolabs/lz-definitions";
import { ExecutorOptionType } from "@layerzerolabs/lz-v2-utilities";
import { TwoWayConfig, generateConnectionsConfig } from "@layerzerolabs/metadata-tools";
import { OAppEnforcedOption, OmniPointHardhat } from "@layerzerolabs/toolbox-hardhat";

const bscTestnetBridgeContract: OmniPointHardhat = {
  eid: EndpointId.BSC_V2_TESTNET,
  contractName: "TargetMessenger",
};

const bscMainnetBridgeContract: OmniPointHardhat = {
  eid: EndpointId.BSC_V2_MAINNET,
  contractName: "TargetMessenger",
};

const outbePrivnetBridgeContract: OmniPointHardhat = {
  eid: 40512 as EndpointId,
  contractName: "OriginMessenger",
};

const outbeDevnetBridgeContract: OmniPointHardhat = {
  eid: 40712 as EndpointId,
  contractName: "OriginMessenger",
};

const outbeTestnetBridgeContract: OmniPointHardhat = {
  eid: 40812 as EndpointId,
  contractName: "OriginMessenger",
};

const outbeTestnetNewBridgeContract: OmniPointHardhat = {
  eid: 40912 as EndpointId,
  contractName: "OriginMessenger",
};

// Bridge msg types (BridgeMsgCodec). Types 1/8/9 (BIDS/ISSUANCE/REFUND) are sized on-chain by
// LzGasEstimator, which overrides this value — kept only as a fallback floor. Types 6/10 carry
// nested array-scaled sends; static best-effort, an oversized batch reverts and Pattern A retries.
const BRIDGE_ENFORCED_OPTIONS: OAppEnforcedOption[] = [
  { msgType: 1, optionType: ExecutorOptionType.LZ_RECEIVE, gas: 300000, value: 0 },
  { msgType: 2, optionType: ExecutorOptionType.LZ_RECEIVE, gas: 200000, value: 0 },
  { msgType: 3, optionType: ExecutorOptionType.LZ_RECEIVE, gas: 200000, value: 0 },
  { msgType: 4, optionType: ExecutorOptionType.LZ_RECEIVE, gas: 250000, value: 0 },
  { msgType: 5, optionType: ExecutorOptionType.LZ_RECEIVE, gas: 200000, value: 0 },
  { msgType: 6, optionType: ExecutorOptionType.LZ_RECEIVE, gas: 1000000, value: 0 },
  { msgType: 7, optionType: ExecutorOptionType.LZ_RECEIVE, gas: 200000, value: 0 },
  { msgType: 8, optionType: ExecutorOptionType.LZ_RECEIVE, gas: 2000000, value: 0 },
  { msgType: 9, optionType: ExecutorOptionType.LZ_RECEIVE, gas: 400000, value: 0 },
  { msgType: 10, optionType: ExecutorOptionType.LZ_RECEIVE, gas: 3000000, value: 0 },
  { msgType: 11, optionType: ExecutorOptionType.LZ_RECEIVE, gas: 200000, value: 0 },
];

// Only pair testnet↔testnet to avoid wrong enforced options.
// BSC Mainnet ↔ Outbe Privnet pathway is not configured yet (no production pair).
const pathways: TwoWayConfig[] = [
  // BSC Testnet ↔ Outbe Testnet
  [
    bscTestnetBridgeContract,
    outbeTestnetBridgeContract,
    [["LayerZero Labs"], []],
    [1, 1],
    [BRIDGE_ENFORCED_OPTIONS, BRIDGE_ENFORCED_OPTIONS],
  ],
  // BSC Testnet ↔ Outbe Testnet (new)
  [
    bscTestnetBridgeContract,
    outbeTestnetNewBridgeContract,
    [["LayerZero Labs"], []],
    [1, 1],
    [BRIDGE_ENFORCED_OPTIONS, BRIDGE_ENFORCED_OPTIONS],
  ],
  // BSC Testnet ↔ Outbe Devnet
  [
    bscTestnetBridgeContract,
    outbeDevnetBridgeContract,
    [["LayerZero Labs"], []],
    [1, 1],
    [BRIDGE_ENFORCED_OPTIONS, BRIDGE_ENFORCED_OPTIONS],
  ],
];

// Exported for lz:set-enforced-options task
export { BRIDGE_ENFORCED_OPTIONS, pathways };

export default async function () {
  const connections = await generateConnectionsConfig(pathways);
  return {
    contracts: [
      { contract: bscTestnetBridgeContract },
      { contract: bscMainnetBridgeContract },
      { contract: outbeTestnetBridgeContract },
      { contract: outbeTestnetNewBridgeContract },
      { contract: outbePrivnetBridgeContract },
      { contract: outbeDevnetBridgeContract },
    ],
    connections,
  };
}
