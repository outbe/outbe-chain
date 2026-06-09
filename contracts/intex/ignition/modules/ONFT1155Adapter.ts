import { buildModule } from "@nomicfoundation/hardhat-ignition/modules";

/**
 * Ignition module for deploying ONFT1155Adapter
 * 
 * Required parameters:
 * - token: Address of the ERC1155Bridgeable token (e.g., IntexNFT1155)
 * - lzEndpoint: LayerZero V2 EndpointV2 address
 * - delegate: Owner/delegate address for the adapter
 * - outbeEid: LayerZero endpoint ID for the Outbe chain
 *
 * After deployment:
 * 1. Grant RELAYER_ROLE on the token contract to the adapter address
 * 2. Set peers for cross-chain communication using setPeer()
 * 3. Configure enforced options using setEnforcedOptions()
 */
export default buildModule("ONFT1155AdapterModule", (m) => {
  const token = m.getParameter("token");
  const lzEndpoint = m.getParameter("lzEndpoint");
  const delegate = m.getParameter("delegate");
  const outbeEid = m.getParameter("outbeEid");
  const adapter = m.contract("ONFT1155Adapter", [
    token,
    lzEndpoint,
    delegate,
    outbeEid,
  ]);

  return { adapter };
});


