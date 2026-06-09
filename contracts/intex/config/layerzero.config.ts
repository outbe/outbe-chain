// LayerZero config for ONFT1155Adapter
// Used by lz:set-enforced-options task
import { EndpointId } from "@layerzerolabs/lz-definitions";
import { ExecutorOptionType } from "@layerzerolabs/lz-v2-utilities";
import { TwoWayConfig, generateConnectionsConfig } from "@layerzerolabs/metadata-tools";
import { OAppEnforcedOption, OmniPointHardhat } from "@layerzerolabs/toolbox-hardhat";

const bscTestnetContract: OmniPointHardhat = {
  eid: EndpointId.BSC_V2_TESTNET,
  contractName: "ONFT1155Adapter",
};

const bscMainnetContract: OmniPointHardhat = {
  eid: EndpointId.BSC_V2_MAINNET,
  contractName: "ONFT1155Adapter",
};

const outbePrivnetContract: OmniPointHardhat = {
  eid: 40512 as EndpointId,
  contractName: "ONFT1155Adapter",
};

const outbeDevnetContract: OmniPointHardhat = {
  eid: 40712 as EndpointId,
  contractName: "ONFT1155Adapter",
};

const outbeTestnetContract: OmniPointHardhat = {
  eid: 40812 as EndpointId,
  contractName: "ONFT1155Adapter",
};

const outbeTestnetNewContract: OmniPointHardhat = {
  eid: 40912 as EndpointId,
  contractName: "ONFT1155Adapter",
};

// Gas for _lzReceive on destination (credit + event)
const EVM_ENFORCED_OPTIONS: OAppEnforcedOption[] = [
  {
    msgType: 1, // SEND
    optionType: ExecutorOptionType.LZ_RECEIVE,
    gas: 100000,
    value: 0,
  },
  {
    msgType: 2, // SEND_AND_COMPOSE
    optionType: ExecutorOptionType.LZ_RECEIVE,
    gas: 100000,
    value: 0,
  },
];

const pathways: TwoWayConfig[] = [
  // BSC Testnet ↔ Outbe (bscTestnet can pair with any Outbe network)
  [
    bscTestnetContract,
    outbeTestnetContract,
    [["LayerZero Labs"], []],
    [1, 1],
    [EVM_ENFORCED_OPTIONS, EVM_ENFORCED_OPTIONS],
  ],
  [
    bscTestnetContract,
    outbeTestnetNewContract,
    [["LayerZero Labs"], []],
    [1, 1],
    [EVM_ENFORCED_OPTIONS, EVM_ENFORCED_OPTIONS],
  ],
  [
    bscTestnetContract,
    outbeDevnetContract,
    [["LayerZero Labs"], []],
    [1, 1],
    [EVM_ENFORCED_OPTIONS, EVM_ENFORCED_OPTIONS],
  ],
  [
    bscTestnetContract,
    outbePrivnetContract,
    [["LayerZero Labs"], []],
    [1, 1],
    [EVM_ENFORCED_OPTIONS, EVM_ENFORCED_OPTIONS],
  ],
];

// Exported for lz:set-enforced-options task (prod enforced options)
export { EVM_ENFORCED_OPTIONS, pathways };

export default async function () {
  const connections = await generateConnectionsConfig(pathways);
  return {
    contracts: [
      { contract: bscTestnetContract },
      { contract: bscMainnetContract },
      { contract: outbeTestnetContract },
      { contract: outbeTestnetNewContract },
      { contract: outbePrivnetContract },
      { contract: outbeDevnetContract },
    ],
    connections,
  };
}
