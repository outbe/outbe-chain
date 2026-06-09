import { buildModule } from "@nomicfoundation/hardhat-ignition/modules";

export default buildModule("EscrowAdapterModule", (m) => {
  const deployer = m.getParameter("deployer");
  const bridger = m.getParameter("bridger");

  const escrowAdapter = m.contract("EscrowAdapter", [
    deployer,
    bridger,
  ]);

  return { escrowAdapter };
});
