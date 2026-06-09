import { buildModule } from "@nomicfoundation/hardhat-ignition/modules";

export default buildModule("IntexAuctionModule", (m) => {
  const deployer = m.getParameter("deployer");
  const bridger = m.getParameter("bridger");

  const auction = m.contract("IntexAuction", [
    deployer,
    bridger,
  ]);

  return { auction };
});


