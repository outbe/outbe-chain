import { buildModule } from "@nomicfoundation/hardhat-ignition/modules";

export default buildModule("TargetMessengerModule", (m) => {
  const lzEndpoint = m.getParameter("lzEndpoint");
  const delegate = m.getParameter("delegate");
  const outbeEid = m.getParameter("outbeEid");

  // TargetMessenger links the external BridgeMsgCodec library — deploy it first. The library
  // holds the variable-length decoders (decodeIssuanceInstructions, decodeRefundInstructions)
  // that would otherwise inline into TargetMessenger and push it past the EIP-170 runtime-size
  // limit.
  const bridgeMsgCodec = m.library("BridgeMsgCodec");

  const bnbAdapter = m.contract(
    "TargetMessenger",
    [lzEndpoint, delegate, outbeEid],
    {
      libraries: { BridgeMsgCodec: bridgeMsgCodec },
    },
  );

  return { bnbAdapter, bridgeMsgCodec };
});
