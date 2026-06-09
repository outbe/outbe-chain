import { buildModule } from "@nomicfoundation/hardhat-ignition/modules";

export default buildModule("OriginMessengerModule", (m) => {
  const lzEndpoint = m.getParameter("lzEndpoint");
  const delegate = m.getParameter("delegate");
  const bnbEid = m.getParameter("bnbEid");

  const outbeAdapter = m.contract("OriginMessenger", [
    lzEndpoint,
    delegate,
    bnbEid,
  ]);

  return { outbeAdapter };
});
