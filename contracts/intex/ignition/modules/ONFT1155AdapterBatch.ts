import { buildModule } from "@nomicfoundation/hardhat-ignition/modules";

/**
 * Ignition module for deploying ONFT1155AdapterBatch
 * 
 * Required parameters:
 * - token: Address of the ERC1155Bridgeable token (e.g., IntexNFT1155)
 * - lzEndpoint: LayerZero V2 EndpointV2 address
 * - delegate: Owner/delegate address for the adapter
 * 
 * After deployment:
 * 1. Grant RELAYER_ROLE on the token contract to the adapter address
 * 2. Set peers for cross-chain communication using setPeer()
 * 3. Configure enforced options using setEnforcedOptions()
 * 
 * Benefits of batch adapter:
 * - Pay ONE LayerZero messaging fee for multiple token transfers
 * - Atomic transfer - all tokens transfer together or none do
 * - ~76% cheaper than separate transactions for 5 token types
 */
export default buildModule("ONFT1155AdapterBatchModule", (m) => {
  const token = m.getParameter("token");
  const lzEndpoint = m.getParameter("lzEndpoint");
  const delegate = m.getParameter("delegate");

  const batchAdapter = m.contract("ONFT1155AdapterBatch", [
    token,
    lzEndpoint,
    delegate,
  ]);

  return { batchAdapter };
});


