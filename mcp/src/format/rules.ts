import { formatUnits } from "viem";
import {
  credisStateName,
  currencyLabel,
  dayTypeName,
  gemStateName,
  proposalStatusName,
  statusName,
  validatorStatusName,
} from "../registry.js";
import { parseDataUri } from "./datauri.js";
import { epochIso } from "./time.js";

/**
 * Sources of each unit:
 *  - WorldwideDay u32 YYYYMMDD .......... crates/core/common/src/worldwideday.rs
 *  - native COEN amounts at 1e18 ........ explicit contract/function boundaries
 *  - protocol monetary amounts at 1e6 ... crates/blockchain/primitives/src/units.rs
 *  - asset-native amounts, raw .......... the asset's own decimals (Credis, VaultRouter, settlement)
 *  - Credis annual currency rate at 1e6 . Oracle/Credis contract
 *  - generic prices/ratios at 1e18 ...... their owning protocol modules
 *  - status / day_type enums ............ crates/core/metadosis/src/schema.rs
 */
const DATE_RE = /(worldwideday|^wwd$|^wwds$|^date$|^day$)/i;
const SIX_DECIMAL_AMOUNT_RE = /(minor$|amount|stake|balance|pledged|reward)/i;
const ASSET_UNIT_AMOUNT_RE =
  /^(principal|outstandingPrincipal|principalPaid|principalWrittenOff|interestPaid|interestAccrued|interest|payment|amount)Minor$/;
const SIX_DECIMAL_RATE_RE = /currencyrate|^policyrate$/i;
const DIMENSIONLESS_FP18_RE = /(rewardband|minvalidperwindow|slashfraction)/i;
const GENERIC_FP18_RE = /(vwap|twap|rate|price|volume|peakprice|currentvalue|nominalprice|maxscurve)/i;
const TIME_RE = /(at$|time$|timestamp$|start$|end$|date$|duedate$|paidat$|deadline$)/i;

/** Reads whose every uint256 output is native COEN at 1e18. */
const NATIVE_COEN_READS = new Set([
  "staking.getStake",
  "staking.getTotalStaked",
  "agentreward.getClaimableBalance",
  "agentreward.getPoolClaimableBalance",
]);
/** Fields that are native COEN at 1e18 inside an otherwise mixed record. */
const NATIVE_COEN_FIELDS = new Set(["validatorset.validatorByAddress.stake", "validatorset.validatorByIndex.stake"]);

export interface ScalarContext {
  contractName?: string;
  functionName?: string;
  /** The enclosing tuple's `internalType`, e.g. `struct IGovernance.Proposal`. */
  enclosingTupleType?: string;
  marketDecimals?: number;
}

export interface Scalar {
  name: string;
  type: string;
  value: unknown;
  context: ScalarContext;
}

interface Rule {
  when: (s: Scalar) => boolean;
  render: (s: Scalar) => unknown;
}

const uint = (bits: number) => (s: Scalar) => s.type === `uint${bits}`;
const uint256Named = (re: RegExp) => (s: Scalar) => s.type === "uint256" && re.test(s.name);
const scaled = (decimals: number) => (s: Scalar) => {
  const v = s.value as bigint;
  return { raw: v.toString(), value: formatUnits(v, decimals) };
};
const coded = (label: (v: number) => string) => (s: Scalar) => {
  const v = Number(s.value);
  return { code: v, name: label(v) };
};

function formatWwd(v: number): string {
  const y = Math.floor(v / 10_000);
  const m = Math.floor((v / 100) % 100);
  const d = v % 100;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${y}-${pad(m)}-${pad(d)}`;
}

/** A bare `status` byte is a proposal's or a validator's where its owner says so, else a WorldwideDay's. */
function statusLabel(s: Scalar): (v: number) => string {
  if (/\bIGovernance\./.test(s.context.enclosingTupleType ?? "")) return proposalStatusName;
  if (s.context.contractName === "validatorset") return validatorStatusName;
  return statusName;
}

/** Names a `state` code by the contract whose struct carries it. Any other keeps the bare code. */
function lifecycleState(s: Scalar): unknown {
  const v = Number(s.value);
  const owner =
    /\bI(Gem|Credis)\./.exec(s.context.enclosingTupleType ?? "")?.[1]?.toLowerCase() ?? s.context.contractName;
  if (owner === "gem") return { code: v, name: gemStateName(v) };
  if (owner === "credis") return { code: v, name: credisStateName(v) };
  return { code: v };
}

function isNativeCoen(s: Scalar): boolean {
  const read = `${s.context.contractName}.${s.context.functionName}`;
  return s.type === "uint256" && (NATIVE_COEN_READS.has(read) || NATIVE_COEN_FIELDS.has(`${read}.${s.name}`));
}

/** The first rule that matches renders the value. Order matters: specific rules precede conventions. */
const RULES: Rule[] = [
  { when: (s) => s.name === "maxRetainedWorldwideDays", render: (s) => Number(s.value) },
  { when: (s) => uint(32)(s) && DATE_RE.test(s.name), render: (s) => ({ wwd: Number(s.value), date: formatWwd(Number(s.value)) }) },
  { when: (s) => uint(8)(s) && s.name === "status", render: (s) => coded(statusLabel(s))(s) },
  { when: (s) => uint(8)(s) && s.name === "dayType", render: coded(dayTypeName) },
  { when: (s) => uint(8)(s) && s.name === "state", render: lifecycleState },
  { when: (s) => uint(16)(s) && /currency/i.test(s.name), render: (s) => currencyLabel(Number(s.value)) },
  { when: uint256Named(DIMENSIONLESS_FP18_RE), render: scaled(18) },
  { when: isNativeCoen, render: scaled(18) },
  {
    when: (s) => s.type === "uint256" && (SIX_DECIMAL_RATE_RE.test(s.name) || s.context.functionName === "getPolicyRate"),
    render: scaled(6),
  },
  { when: uint256Named(ASSET_UNIT_AMOUNT_RE), render: (s) => (s.value as bigint).toString() },
  { when: (s) => s.context.contractName === "vaultrouter" && uint256Named(/amount/i)(s), render: (s) => (s.value as bigint).toString() },
  { when: uint256Named(SIX_DECIMAL_AMOUNT_RE), render: scaled(6) },
  { when: uint256Named(GENERIC_FP18_RE), render: (s) => scaled(s.context.marketDecimals ?? 18)(s) },
  {
    when: (s) => uint(64)(s) && TIME_RE.test(s.name) && !/height$|block$/i.test(s.name),
    render: (s) => epochIso(s.value as bigint),
  },
  { when: (s) => s.type === "string" && typeof s.value === "string", render: (s) => parseDataUri(s.value as string) },
  { when: (s) => typeof s.value === "bigint", render: (s) => (s.value as bigint).toString() },
];

/** Formats a single (non-array, non-tuple) value by its ABI name and type. */
export function formatScalar(scalar: Scalar): unknown {
  const rule = RULES.find((candidate) => candidate.when(scalar));
  return rule ? rule.render(scalar) : scalar.value;
}
