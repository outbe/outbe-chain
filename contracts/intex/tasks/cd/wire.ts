import { task } from "hardhat/config";
import {
  auctionWireAction,
  escrowWireAction,
  targetBridgeWireAction,
  originBridgeWireAction,
} from "./wireRouters.js";
import {
  burnerGrantRolesAction,
  grantRelayerRoleAction,
} from "./wireRoles.js";
import {
  systemGrantRolesAction,
  intexFactoryAssertRelayerRoleAction,
} from "./wireSystemRoles.js";

function wireTask<A>(
  name: string,
  description: string,
  options: [string, string][],
  action: (args: A, hre: unknown) => Promise<void>,
) {
  const builder = task(name, description);
  for (const [optionName, optionDescription] of options) {
    builder.addOption({ name: optionName, description: optionDescription, defaultValue: "" });
  }
  return builder.setAction(async () => ({
    default: (args, hre) => action(args as A, hre),
  })).build();
}

export const wireTasks = [
  wireTask("auction-wire", "Wire Auction to EscrowAdapter", [
    ["intexAuctionContract", "Auction contract address"],
    ["escrowContract", "EscrowAdapter contract address"],
  ], auctionWireAction),
  wireTask("escrow-wire", "Wire EscrowAdapter to Auction and external contracts", [
    ["escrowContract", "EscrowAdapter contract address"],
    ["intexAuctionContract", "Auction contract address"],
    ["compactContract", "TheCompact contract address"],
    ["paymentToken", "PaymentToken address"],
  ], escrowWireAction),
  wireTask("target-bridge-wire", "Wire TargetRouter to Auction, Intex, and EscrowAdapter", [
    ["bridgeContract", "TargetRouter contract address"],
    ["intexAuctionContract", "Auction contract address"],
    ["intexContract", "IntexNFT1155 contract address"],
    ["escrowContract", "EscrowAdapter contract address"],
  ], targetBridgeWireAction),
  wireTask("origin-bridge-wire", "Wire OriginRouter to Desis + IntexFactory", [
    ["bridgeContract", "OriginRouter contract address"],
    ["desisContract", "Desis contract address"],
    ["intexFactoryContract", "IntexFactory contract address"],
  ], originBridgeWireAction),
  wireTask(
    "outbe-system-grant-roles",
    "Grant precompile-caller roles: DESIS_ROLE + INTEX_FACTORY_ROLE to the begin-block system caller; INTEX_FACTORY_ROLE + RELAYER_ROLE to the Desis precompile",
    [
      ["bridgeContract", "OriginRouter contract address"],
      ["intexContract", "IntexNFT1155 contract address on Outbe"],
      ["systemAddress", "Outbe begin-block system caller (OUTBE_SYSTEM_TX_ADDRESS)"],
      ["desisContract", "Desis precompile address (clearing-tick issuance frame)"],
    ], systemGrantRolesAction,
  ),
  wireTask(
    "intex-factory-assert-relayer-role",
    "Assert the Desis precompile and the begin-block system caller hold RELAYER_ROLE on IntexNFT1155 (fails the deploy if missing)",
    [
      ["intexContract", "IntexNFT1155 contract address on Outbe"],
      ["desisContract", "Desis precompile address (createSeries caller)"],
      ["systemAddress", "Outbe begin-block system caller (markCalled)"],
    ], intexFactoryAssertRelayerRoleAction,
  ),
  wireTask("burner-grant-roles", "Grant BURNER_ROLE on IntexNFT1155 to the IntexFactory and GemFactory precompiles", [
    ["intexFactory", "IntexFactory precompile address on Outbe"],
    ["gemFactory", "GemFactory precompile address on Outbe"],
    ["intexContract", "IntexNFT1155 contract address on Outbe"],
  ], burnerGrantRolesAction),
  wireTask("grant-relayer-role", "Grant RELAYER_ROLE on an app contract (IntexAuction, EscrowAdapter, IntexNFT1155) to a bridge client", [
    ["token", "App contract that gates inbound calls by RELAYER_ROLE"],
    ["adapter", "Bridge client to grant RELAYER_ROLE to (router or NFT bridge)"],
    ["contract", "App contract name: IntexAuction | EscrowAdapter | IntexNFT1155 (default: IntexNFT1155)"],
  ], grantRelayerRoleAction),
];
