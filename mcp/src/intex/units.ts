import { formatUnits, parseUnits } from "viem";

const SCALE_1E6 = 1_000_000n;
const NATIVE_UNITS_PER_PROTOCOL_UNIT = 1_000_000_000_000n;

/** Convert the protocol-6 auction basis and rate into the native-18 WCOEN lock. */
export function wcoenLockAmount(units: bigint, promisLoadProtocol: bigint, bidRate: bigint): bigint {
  return ((units * promisLoadProtocol * bidRate) / SCALE_1E6) * NATIVE_UNITS_PER_PROTOCOL_UNIT;
}

export function priced(minor: bigint) {
  return { raw: minor.toString(), value: formatUnits(minor, 6), scale: "1e6 ISO stable-unit" };
}

/** Bid rate as a fraction of strike ("0.8" = 80%) to the uint32 1e6 fixed-point the contract expects. */
export function toBidRate(rate: string): bigint {
  const raw = parseUnits(rate, 6);
  if (raw < 0n || raw > SCALE_1E6) throw new Error(`bid rate ${rate} must be 0..1 (0-100% of strike)`);
  return raw;
}
