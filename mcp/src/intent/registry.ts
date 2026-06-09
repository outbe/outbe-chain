import { type Abi, parseAbi } from "viem";

/**
 * ABI + constants for the intent (ERC-7683 LayerZeroRouter) tools.
 *
 * ABIs are embedded as viem human-readable signatures (no Solidity compile step),
 * matching the convention in `src/registry.ts`. Source of truth:
 *  - contracts/intent/src/router/origin/OriginSettlerBase.sol  (open, openOrders, resolve)
 *  - contracts/intent/src/router/common/OrderStatusStorage.sol (orderStatus, status constants)
 *  - contracts/intent/src/libs/OrderEncoder.sol                (OrderData layout, type hash, id)
 */

export const DEFAULT_ROUTER = "0xC846a86D4FE91a43E900a7a3bd5BE23ED2C30492";
export const DEFAULT_FILL_DEADLINE_SECONDS = 120; // 120s

/**
 * Supported networks besides `outbe` (always the connected ctx). Resolved by
 * name or chain id — no RPC URLs, no aliases; the model normalizes natural
 * language ("бсц", "BSC testnet") to `bsc`. Add a row to support another chain.
 */
export interface NetworkDef {
  name: string;
  chainId: number;
  rpc: string;
}

export const NETWORKS: NetworkDef[] = [
  { name: "bsc-testnet", chainId: 97, rpc: "https://bsc-testnet-rpc.publicnode.com" },
  { name: "outbe-testnet", chainId: 54322345, rpc: "https://rpc.testnet.outbe.net" },
];

export const ROUTER_ABI: Abi = parseAbi([
  "function open((uint32 fillDeadline, bytes32 orderDataType, bytes orderData) order) payable",
  "function refund((uint32 fillDeadline, bytes32 orderDataType, bytes orderData)[] orders) payable",
  "function openOrders(bytes32 orderId) view returns (bytes)",
  "function orderStatus(bytes32 orderId) view returns (bytes32)",
  "function destinationOrderStatus(bytes32 orderId) view returns (bytes32)",
  "function isValidNonce(address from, uint256 nonce) view returns (bool)",
  "function quote(uint32 dstDomain, bytes payload, bool payInLzToken) view returns ((uint256 nativeFee, uint256 lzTokenFee))",
]);

export const ERC20_ABI: Abi = parseAbi([
  "function decimals() view returns (uint8)",
  "function symbol() view returns (string)",
  "function balanceOf(address account) view returns (uint256)",
  "function allowance(address owner, address spender) view returns (uint256)",
  "function approve(address spender, uint256 amount) returns (bool)",
]);
