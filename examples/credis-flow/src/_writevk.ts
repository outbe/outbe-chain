// Regenerate the commitment-nullifier VK locally from a sibling
// outbe-circuits checkout using the bb.js version pinned in this package.
// Run this after a bb / nargo nightly bump to cross-check against the
// canonical VK shipped in outbe-zk-canonical (`res/vks/commitment_nullifier.vk`).
//
//   npx tsx src/_writevk.ts

import { writeFileSync } from "fs";
import { Barretenberg, UltraHonkBackend } from "@aztec/bb.js";
import { loadCircuit } from "./shielded.js";

const TARGET_VK = new URL(
  "../../../../outbe-circuits/crates/outbe-zk-canonical/res/vks/commitment_nullifier.vk",
  import.meta.url,
);

async function main() {
  const circuit = loadCircuit();
  const api = await Barretenberg.new({ threads: 1 });
  try {
    const backend = new UltraHonkBackend(circuit.bytecode, api);
    const vk = await backend.getVerificationKey({ verifierTarget: "evm" });
    writeFileSync(TARGET_VK, vk);
    console.log(`Wrote ${vk.length} bytes to ${TARGET_VK.pathname}`);
  } finally {
    await api.destroy();
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
