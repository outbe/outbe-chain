// Regenerate the commitment-nullifier VK locally from the pinned
// `outbe-circuits` bytecode and write it to the canonical-asset cache.
//
//   npx tsx src/_writevk.ts                      # write to default cache slot
//   npx tsx src/_writevk.ts <output-file-path>   # override target
//
// Run after a bb / nargo nightly bump to cross-check against the
// canonical VK shipped in `outbe-zk-canonical` (downloaded into the
// same cache slot by `loadCommitmentNullifierVk()`). Byte-for-byte
// equality is the proof / verifier compatibility signal.

import { mkdirSync, writeFileSync } from "fs";
import { dirname, resolve } from "path";
import { Barretenberg, UltraHonkBackend } from "@aztec/bb.js";
import {
  commitmentNullifierVkCachePath,
  loadCircuit,
  OUTBE_CIRCUITS_VERSION,
} from "./shielded.js";

async function main() {
  const targetPath = process.argv[2]
    ? resolve(process.argv[2])
    : commitmentNullifierVkCachePath();

  const circuit = await loadCircuit();
  const api = await Barretenberg.new({ threads: 1 });
  try {
    const backend = new UltraHonkBackend(circuit.bytecode, api);
    const vk = await backend.getVerificationKey({ verifierTarget: "evm" });
    mkdirSync(dirname(targetPath), { recursive: true });
    writeFileSync(targetPath, vk);
    console.log(
      `[outbe-circuits ${OUTBE_CIRCUITS_VERSION}] wrote ${vk.length} bytes to ${targetPath}`,
    );
  } finally {
    await api.destroy();
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
