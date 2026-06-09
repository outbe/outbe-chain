import { type AbiFunction, type AbiParameter, formatUnits } from "viem";
import { currencyLabel, dayTypeName, gemStateName, statusName } from "./registry.js";

/**
 * Turn viem-decoded contract return values into human-readable JSON, driven by
 * the ABI output parameter names (registry.ts keeps them verbatim).
 *
 * Formatting sources:
 *  - WorldwideDay u32 YYYYMMDD .......... crates/core/common/src/worldwideday.rs
 *  - *_minor / amounts at 1e18 .......... crates/blockchain/primitives/src/units.rs
 *  - status / day_type enums ............ crates/core/metadosis/src/schema.rs
 */

const DATE_RE = /(worldwideday|^wwd$|^wwds$|^date$|^day$)/i;
const MINOR_RE =
  /(minor$|amount|stake|balance|vwap|twap|rate|price|volume|pledged|reward|peakprice|currentvalue|nominalprice|maxscurve)/i;
const TIME_RE = /(at$|time$|timestamp$|start$|end$|date$|duedate$|paidat$)/i;

function formatWwd(v: number): string {
  const y = Math.floor(v / 10_000);
  const m = Math.floor((v / 100) % 100);
  const d = v % 100;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${y}-${pad(m)}-${pad(d)}`;
}

function toIso(epoch: bigint | number): string {
  const sec = Number(epoch);
  if (!Number.isFinite(sec) || sec <= 0) return "n/a";
  return new Date(sec * 1000).toISOString();
}

function parseDataUri(s: string): unknown {
  const marker = "data:application/json";
  if (!s.startsWith(marker)) return s;
  const comma = s.indexOf(",");
  if (comma < 0) return s;
  const meta = s.slice(0, comma);
  let payload = s.slice(comma + 1);
  try {
    if (meta.includes(";base64")) {
      payload = Buffer.from(payload, "base64").toString("utf8");
    }
    return JSON.parse(payload);
  } catch {
    return s;
  }
}

function isUint(type: string, bits?: number): boolean {
  if (bits) return type === `uint${bits}`;
  return type.startsWith("uint");
}

/** Format a single (non-array, non-tuple) scalar value by name + type. */
function formatScalar(name: string, type: string, value: unknown): unknown {
  const n = name ?? "";

  if (isUint(type, 32) && DATE_RE.test(n)) {
    const v = Number(value);
    return { wwd: v, date: formatWwd(v) };
  }
  if (isUint(type, 8) && n === "status") {
    const v = Number(value);
    return { code: v, name: statusName(v) };
  }
  if (isUint(type, 8) && n === "dayType") {
    const v = Number(value);
    return { code: v, name: dayTypeName(v) };
  }
  if (isUint(type, 8) && n === "state") {
    const v = Number(value);
    return { code: v, name: gemStateName(v) };
  }
  if (isUint(type, 16) && /currency/i.test(n)) {
    return currencyLabel(Number(value));
  }
  if (type === "uint256" && MINOR_RE.test(n)) {
    // 1e18 fixed-point. The unit is context-dependent: native-token amounts
    // (balances, stake, rewards, nominal) are COEN, but currency-denominated
    // amounts (e.g. issuance_amount_minor) are in the paired *_currency (USD,
    // ...). So expose the scaled decimal without asserting a unit.
    const v = value as bigint;
    return { raw: v.toString(), value: formatUnits(v, 18) };
  }
  if (isUint(type, 64) && TIME_RE.test(n) && !/height$|block$/i.test(n)) {
    return { epoch: Number(value), iso: toIso(value as bigint) };
  }
  if (type === "string" && typeof value === "string") {
    return parseDataUri(value);
  }
  if (typeof value === "bigint") return value.toString();
  return value;
}

/** Recursively format a value against its ABI parameter metadata. */
export function formatParam(param: AbiParameter, value: unknown): unknown {
  const { type } = param;

  if (type.endsWith("[]")) {
    const base = { ...param, type: type.slice(0, -2) } as AbiParameter;
    return Array.isArray(value) ? value.map((v) => formatParam(base, v)) : value;
  }

  if (type === "tuple" && "components" in param && param.components) {
    const out: Record<string, unknown> = {};
    param.components.forEach((c, i) => {
      const sub =
        value && typeof value === "object" && c.name && c.name in (value as object)
          ? (value as Record<string, unknown>)[c.name]
          : (value as unknown[])[i];
      out[c.name || String(i)] = formatParam(c, sub);
    });
    return out;
  }

  return formatScalar(param.name ?? "", type, value);
}

/**
 * Humanize a decoded function result. `result` is what viem returns from
 * `readContract`/`decodeFunctionResult`: a scalar for a single output, an array
 * for multiple outputs.
 */
export function humanizeReturn(fn: AbiFunction, result: unknown): unknown {
  const outputs = fn.outputs ?? [];
  if (outputs.length === 0) return null;
  if (outputs.length === 1) {
    const p = outputs[0];
    const formatted = formatParam(p, result);
    return p.name ? { [p.name]: formatted } : formatted;
  }
  const arr = result as unknown[];
  const out: Record<string, unknown> = {};
  outputs.forEach((p, i) => {
    out[p.name || String(i)] = formatParam(p, arr[i]);
  });
  return out;
}

/** JSON.stringify replacer that renders bigint as a decimal string. */
export function bigintReplacer(_key: string, value: unknown): unknown {
  return typeof value === "bigint" ? value.toString() : value;
}

/** Stringify any value for MCP text content, bigint-safe and pretty. */
export function toJson(value: unknown): string {
  return JSON.stringify(value, bigintReplacer, 2);
}
