import { buildModule } from "@nomicfoundation/hardhat-ignition/modules";

export default buildModule("IntexNFT1155Module", (m) => {
  const defaultAdmin = m.getParameter("defaultAdmin");
  const bridger = m.getParameter("bridger");

  // IntexNFT1155 links the external IntexMetadata library — deploy it first.
  const intexMetadata = m.library("IntexMetadata");

  const intex1155 = m.contract(
    "IntexNFT1155",
    [defaultAdmin, bridger],
    {
      libraries: { IntexMetadata: intexMetadata },
    },
  );

  return { intex1155, intexMetadata };
});
