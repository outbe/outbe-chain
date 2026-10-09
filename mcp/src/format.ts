export { type DecodedDataUri, parseDataUri } from "./format/datauri.js";
export { type ReturnFormatContext } from "./format/oracle-scale.js";
export { formatParam, humanizeReturn } from "./format/walk.js";

/** JSON.stringify replacer that renders bigint as a decimal string. */
export function bigintReplacer(_key: string, value: unknown): unknown {
  return typeof value === "bigint" ? value.toString() : value;
}

/** Stringify any value for MCP text content, bigint-safe and pretty. */
export function toJson(value: unknown): string {
  return JSON.stringify(value, bigintReplacer, 2);
}
