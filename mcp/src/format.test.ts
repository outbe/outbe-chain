import assert from "node:assert/strict";
import test from "node:test";
import { parseDataUri } from "./format.js";

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
