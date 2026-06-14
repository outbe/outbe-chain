// Regenerate the commitment-nullifier VK locally from the pinned
// `outbe-circuits` bytecode and write it to the canonical-asset cache.
//
//   npx tsx src/_writevk.ts                      # write to default cache slot
//   npx tsx src/_writevk.ts <output-file-path>   # override target
//   npx tsx src/_writevk.ts --force              # regenerate even if cached
//
// Run after a bb / nargo nightly bump to cross-check against the
// canonical VK shipped in `outbe-zk-canonical` (downloaded into the
// same cache slot by `loadCommitmentNullifierVk()`). Byte-for-byte
// equality is the proof / verifier compatibility signal.
//
// When the target VK already exists in the cache dir this is a no-op
// (so it is cheap to wire into `generate-types`); pass `--force` to
// regenerate after a toolchain bump.

import { existsSync, mkdirSync, writeFileSync } from "fs";
import { dirname, resolve } from "path";
import { Barretenberg, UltraHonkBackend } from "@aztec/bb.js";
import {
  commitmentNullifierVkCachePath,
  loadCircuit,
  OUTBE_CIRCUITS_VERSION,
} from "./shielded.js";

async function main() {
  const args = process.argv.slice(2);
  const force = args.includes("--force");
  const positional = args.filter((arg) => arg !== "--force");
  const targetPath = positional[0]
    ? resolve(positional[0])
    : commitmentNullifierVkCachePath();

  if (!force && existsSync(targetPath)) {
    console.log(
      `[outbe-circuits ${OUTBE_CIRCUITS_VERSION}] VK already cached at ${targetPath}; skipping (pass --force to regenerate)`,
    );
    return;
  }

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
