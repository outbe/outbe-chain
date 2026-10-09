import assert from "node:assert/strict";
import test from "node:test";
import { DEFAULT_RPC, loadConfig } from "./config.js";

test("--rpc wins over OUTBE_RPC, which wins over the default", () => {
  const env = { OUTBE_RPC: "https://env.example" };
  assert.equal(loadConfig(["--rpc", "https://flag.example"], env).rpcUrl, "https://flag.example");
  assert.equal(loadConfig(["--rpc"], env).rpcUrl, "https://env.example");
  assert.equal(loadConfig([], {}).rpcUrl, DEFAULT_RPC);
});

test("chain RPCs come only from numbered, non-empty OUTBE_RPC_ variables", () => {
  const { chainRpcs } = loadConfig([], {
    OUTBE_RPC: "https://outbe.example",
    OUTBE_RPC_97: "https://bsc.example",
    OUTBE_RPC_11155111: "",
    OUTBE_RPC_SEPOLIA: "https://named.example",
  });
  assert.deepEqual(chainRpcs, { 97: "https://bsc.example" });
});

test("keys and contract overrides pass through unchanged", () => {
  const config = loadConfig([], {
    OUTBE_PRIVATE_KEY: "0xabc",
    OUTBE_INTENT_ROUTER: "0x01",
    OUTBE_INTEX_ORIGIN_ROUTER: "0x02",
  });
  assert.equal(config.privateKey, "0xabc");
  assert.equal(config.intentRouter, "0x01");
  assert.equal(config.intexOriginRouter, "0x02");
});
