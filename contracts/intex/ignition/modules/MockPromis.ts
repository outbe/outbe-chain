import { buildModule } from "@nomicfoundation/hardhat-ignition/modules";

export default buildModule("MockPromisModule", (m) => {
  const defaultAdmin = m.getParameter("defaultAdmin");

  const mockPromis = m.contract("MockPromis", [
    defaultAdmin,
  ]);

  return { mockPromis };
});

