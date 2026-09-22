import { strict as assert } from "node:assert";
import { spawnSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

test("ABI preparation supports sibling and overridden paths and preserves output on missing input", () => {
  const root = mkdtempSync(join(tmpdir(), "credis-abis-"));
  try {
    const demo = join(root, "chain/scripts/credis-flow");
    mkdirSync(join(demo, "scripts"), { recursive: true });
    cpSync(new URL("./prepare-abis.mjs", import.meta.url), join(demo, "scripts/prepare-abis.mjs"));
    const precompiles = join(root, "chain/contracts/precompiles/abi-export");
    const accounts = join(root, "smart-account/abi-export");
    mkdirSync(precompiles, { recursive: true });
    mkdirSync(accounts, { recursive: true });
    for (const name of ["ICcaRegistry", "IGratis", "IGratisFactory", "IPromis", "IPromisFactory",
      "IGem", "IGemFactory", "ICredis", "ICredisFactory", "IFidelity", "IVaultRouter"]) {
      writeFileSync(join(precompiles, `${name}.json`), "[]");
    }
    for (const name of ["SmartAccountFactory", "ExecutionDelayPolicy", "WithdrawalLimitPolicy", "IEntryPoint", "IERC20"]) {
      writeFileSync(join(accounts, `${name}.json`), "[]");
    }
    const run = (override = "") => spawnSync(process.execPath, ["scripts/prepare-abis.mjs"], {
      cwd: demo, encoding: "utf8", env: { ...process.env, SMART_ACCOUNT_REPO: override },
    });
    const snapshot = () => Object.fromEntries(readdirSync(join(demo, "abi")).sort()
      .map(name => [name, readFileSync(join(demo, "abi", name), "utf8")]));
    const first = run();
    assert.equal(first.status, 0, first.stderr);
    assert.equal(Object.keys(snapshot()).length, 16);
    cpSync(join(root, "smart-account"), join(root, "alternate"), { recursive: true });
    writeFileSync(join(root, "alternate/abi-export/IERC20.json"), '[{"type":"event","name":"Override"}]');
    const overridden = run("../../../alternate");
    assert.equal(overridden.status, 0, overridden.stderr);
    assert.match(snapshot()["IERC20.json"], /Override/);
    const before = snapshot();
    const missing = run(join(root, "missing"));
    assert.notEqual(missing.status, 0);
    assert.match(missing.stderr, /SMART_ACCOUNT_REPO/);
    assert.deepEqual(snapshot(), before);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
