import assert from "node:assert/strict";
import test from "node:test";
import type { AbiParameter } from "viem";
import { formatParam, parseDataUri } from "./format.js";

const encode = (value: string) => Buffer.from(value, "utf8").toString("base64");

test("a base64 metadata document decodes and keeps only the size of its inline image", () => {
  const svg = '<svg xmlns="http://www.w3.org/2000/svg"></svg>';
  const document = {
    name: "Gem 0x3fa2b1...9c0e",
    image: `data:image/svg+xml;base64,${encode(svg)}`,
    attributes: [{ trait_type: "State", value: "Qualified" }],
  };
  const parsed = parseDataUri(`data:application/json;base64,${encode(JSON.stringify(document))}`);
  assert.deepEqual(parsed, {
    ...document,
    image: `<image/svg+xml, ${Buffer.byteLength(svg)} bytes>`,
  });
});

test("an external image link and a plain string pass through untouched", () => {
  const document = { name: "Tribute 0x01", image: "https://example.com/1.png" };
  assert.deepEqual(parseDataUri(`data:application/json;utf8,${JSON.stringify(document)}`), document);
  assert.equal(parseDataUri("Outbe"), "Outbe");
});

test("a lifecycle state is named by the contract whose struct carries it", () => {
  const state = (internalType: string, code: number) =>
    formatParam(
      {
        name: "data",
        type: "tuple",
        internalType,
        components: [{ name: "state", type: "uint8" }],
      } as AbiParameter,
      { state: code },
    );
  assert.deepEqual(state("struct IGem.GemData", 2), { state: { code: 2, name: "Called" } });
  assert.deepEqual(state("struct IGem.GemData", 4), { state: { code: 4, name: "Forfeited" } });
  assert.deepEqual(state("struct ICredis.Credis", 3), { state: { code: 3, name: "Forfeited" } });
  assert.deepEqual(state("struct IOther.Data", 3), { state: { code: 3 } });
  assert.deepEqual(
    formatParam({ name: "state", type: "uint8" } as AbiParameter, 1, { contractName: "credis" }),
    { code: 1, name: "Called" },
  );
});
