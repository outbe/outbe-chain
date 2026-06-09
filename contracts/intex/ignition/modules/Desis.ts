import { buildModule } from "@nomicfoundation/hardhat-ignition/modules";

export default buildModule("DesisModule", (m) => {
  const defaultAdmin = m.getParameter("defaultAdmin");
  const bridger = m.getParameter("bridger");

  const telosis = m.contract("Desis", [
    defaultAdmin,
    bridger,
  ]);

  return { telosis };
});
