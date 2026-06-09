// LayerZero config for ONFT1155AdapterBatch
// Used by lz:set-enforced-options task
import { EndpointId } from "@layerzerolabs/lz-definitions";
import { ExecutorOptionType } from "@layerzerolabs/lz-v2-utilities";
import { TwoWayConfig, generateConnectionsConfig } from "@layerzerolabs/metadata-tools";
import { OAppEnforcedOption, OmniPointHardhat } from "@layerzerolabs/toolbox-hardhat";

const bscTestnetBatchContract: OmniPointHardhat = {
  eid: EndpointId.BSC_V2_TESTNET,
  contractName: "ONFT1155AdapterBatch",
};

const bscMainnetBatchContract: OmniPointHardhat = {
  eid: EndpointId.BSC_V2_MAINNET,
  contractName: "ONFT1155AdapterBatch",
};

const outbePrivnetBatchContract: OmniPointHardhat = {
  eid: 40512 as EndpointId,
  contractName: "ONFT1155AdapterBatch",
};

const outbeDevnetBatchContract: OmniPointHardhat = {
  eid: 40712 as EndpointId,
  contractName: "ONFT1155AdapterBatch",
};

const outbeTestnetBatchContract: OmniPointHardhat = {
  eid: 40812 as EndpointId,
  contractName: "ONFT1155AdapterBatch",
};

const outbeTestnetNewBatchContract: OmniPointHardhat = {
  eid: 40912 as EndpointId,
  contractName: "ONFT1155AdapterBatch",
};

// Gas for batch _lzReceive on destination
// Higher than single-token transfers due to loop processing
const EVM_ENFORCED_OPTIONS: OAppEnforcedOption[] = [
  {
    msgType: 1, // SEND (batchSend)
    optionType: ExecutorOptionType.LZ_RECEIVE,
    gas: 200000, // Base gas for batch receive
    value: 0,
  },
  {
    msgType: 2, // SEND_MULTI (multiSend)
    optionType: ExecutorOptionType.LZ_RECEIVE,
    gas: 250000, // Higher for multi-recipient
    value: 0,
  },
];

const pathways: TwoWayConfig[] = [
  // BSC Testnet ↔ Outbe (bscTestnet can pair with any Outbe network)
  [
    bscTestnetBatchContract,
    outbeTestnetBatchContract,
    [["LayerZero Labs"], []],
    [1, 1],
    [EVM_ENFORCED_OPTIONS, EVM_ENFORCED_OPTIONS],
  ],
  [
    bscTestnetBatchContract,
    outbeTestnetNewBatchContract,
    [["LayerZero Labs"], []],
    [1, 1],
    [EVM_ENFORCED_OPTIONS, EVM_ENFORCED_OPTIONS],
  ],
  [
    bscTestnetBatchContract,
    outbeDevnetBatchContract,
    [["LayerZero Labs"], []],
    [1, 1],
    [EVM_ENFORCED_OPTIONS, EVM_ENFORCED_OPTIONS],
  ],
  [
    bscTestnetBatchContract,
    outbePrivnetBatchContract,
    [["LayerZero Labs"], []],
    [1, 1],
    [EVM_ENFORCED_OPTIONS, EVM_ENFORCED_OPTIONS],
  ],
];

// Exported for lz:set-enforced-options task
export { EVM_ENFORCED_OPTIONS, pathways };

export default async function () {
  const connections = await generateConnectionsConfig(pathways);
  return {
    contracts: [
      { contract: bscTestnetBatchContract },
      { contract: bscMainnetBatchContract },
      { contract: outbeTestnetBatchContract },
      { contract: outbeTestnetNewBatchContract },
      { contract: outbePrivnetBatchContract },
      { contract: outbeDevnetBatchContract },
    ],
    connections,
  };
}
