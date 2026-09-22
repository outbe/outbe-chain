// Aggregates ABI JSON files for the credis-flow demo from canonical sources
// in the chain and smart-account repositories, normalizing every output to {abi: [...]}.
//
// Run via `npm run prepare-abis` (also chained from `npm run generate-types`).

import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(here, "..");
const repoContracts = resolve(projectRoot, "../../contracts");
const smartAccountRoot = process.env.SMART_ACCOUNT_REPO
  ? resolve(process.env.SMART_ACCOUNT_REPO)
  : resolve(projectRoot, "../../../smart-account");
const outDir = resolve(projectRoot, "abi");

// Output name (typechain consumes this as the contract type name) -> source path.
const MAPPING = {
  ICcaRegistry: "precompiles/abi-export/ICcaRegistry.json",
  IGratis: "precompiles/abi-export/IGratis.json",
  IGratisFactory: "precompiles/abi-export/IGratisFactory.json",
  IPromis: "precompiles/abi-export/IPromis.json",
  IPromisFactory: "precompiles/abi-export/IPromisFactory.json",
  IGem: "precompiles/abi-export/IGem.json",
  IGemFactory: "precompiles/abi-export/IGemFactory.json",
  ICredis: "precompiles/abi-export/ICredis.json",
  ICredisFactory: "precompiles/abi-export/ICredisFactory.json",
  IFidelity: "precompiles/abi-export/IFidelity.json",
  IVaultRouter: "precompiles/abi-export/IVaultRouter.json",
  SmartAccountFactory: "abi-export/SmartAccountFactory.json",
  ExecutionDelayPolicy: "abi-export/ExecutionDelayPolicy.json",
  WithdrawalLimitPolicy: "abi-export/WithdrawalLimitPolicy.json",
  IEntryPoint: "abi-export/IEntryPoint.json",
  IERC20: "abi-export/IERC20.json"
};

function extractAbi(name, sourcePath) {
  if (!existsSync(sourcePath)) {
    throw new Error(`prepare-abis: missing source ABI for ${name} at ${sourcePath}. ` +
      "Export the source repository's ABIs; for smart-account, use a sibling checkout or set SMART_ACCOUNT_REPO.");
  }
  const parsed = JSON.parse(readFileSync(sourcePath, "utf8"));
  if (Array.isArray(parsed)) return parsed;
  if (Array.isArray(parsed?.abi)) return parsed.abi;
  throw new Error(
    `prepare-abis: unrecognized ABI shape for ${name} at ${sourcePath} (expected array or {abi: [...]})`,
  );
}

// Validate every input before replacing previously generated ABIs.
const abis = Object.entries(MAPPING).map(([name, relSource]) => {
  const root = relSource.startsWith("precompiles/") ? repoContracts : smartAccountRoot;
  const sourcePath = resolve(root, relSource);
  return { name, sourcePath, abi: extractAbi(name, sourcePath) };
});

if (existsSync(outDir)) rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

for (const { name, sourcePath, abi } of abis) {
  const destPath = resolve(outDir, `${name}.json`);
  writeFileSync(destPath, `${JSON.stringify({ abi }, null, 2)}\n`);
  console.log(`prepare-abis: wrote abi/${name}.json (${abi.length} entries) <- ${sourcePath}`);
}

console.log(`prepare-abis: ${abis.length} ABI files staged in ${outDir}`);
