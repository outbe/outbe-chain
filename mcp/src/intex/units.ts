import { formatUnits, parseUnits } from "viem";

export const PROTOCOL_DECIMALS = 6;
const NATIVE_DECIMALS = 18;
export const NATIVE_UNITS_PER_PROTOCOL_UNIT = 10n ** BigInt(NATIVE_DECIMALS - PROTOCOL_DECIMALS);
const SCALE_1E6 = 1_000_000n;

/** Convert the protocol-6 auction basis and rate into the native-18 WCOEN lock. */
export function wcoenLockAmount(quantity: bigint, promisLoadProtocol: bigint, bidRate: bigint): bigint {
  return ((quantity * promisLoadProtocol * bidRate) / SCALE_1E6) * NATIVE_UNITS_PER_PROTOCOL_UNIT;
}

export function priced(minor: bigint) {
  return { raw: minor.toString(), value: formatUnits(minor, PROTOCOL_DECIMALS), scale: "1e6 ISO stable-unit" };
}

/** Bid rate as a fraction of strike ("0.8" = 80%) to the uint32 1e6 fixed-point the contract expects. */
export function toBidRate(rate: string): bigint {
  const raw = parseUnits(rate, 6);
  if (raw < 0n || raw > SCALE_1E6) throw new Error(`bid rate ${rate} must be 0..1 (0-100% of strike)`);
  return raw;
}
