import { buildModule } from "@nomicfoundation/hardhat-ignition/modules";

export default buildModule("MockPromisLimitModule", (m) => {
  const defaultAdmin = m.getParameter("defaultAdmin");

  // Fully qualified: a same-named MockPromisLimit test stub lives in test/mocks, so the bare
  // name is ambiguous. Pin the future id so the "MockPromisLimitModule#MockPromisLimit" address
  // mapping (scripts/cd/save-addresses.ts) is preserved.
  const mockPromisLimit = m.contract(
    "contracts/outbe/MockPromisLimit.sol:MockPromisLimit",
    [defaultAdmin],
    { id: "MockPromisLimit" },
  );

  return { mockPromisLimit };
});
