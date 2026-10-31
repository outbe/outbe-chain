// Emission-day utilities
// Functions for working with the emission day that keys an auction.
//
// Conventions
// - emissionDay: 8-digit numeric in yyyymmdd format (uint32 on-chain), e.g. 20250924.
//   A plain TypeScript `number` keys every auction. The series id is derived from it.

// =============================================================================
// Parsing
// =============================================================================

/** Parse yyyymmdd format to ISO components */
export function yyyymmddToIso(emissionDay: string): { y: string; m: string; d: string } {
  if (!/^\d{8}$/.test(emissionDay)) {
    throw new Error("emission day must be yyyymmdd, e.g. 20250924");
  }
  return {
    y: emissionDay.slice(0, 4),
    m: emissionDay.slice(4, 6),
    d: emissionDay.slice(6, 8),
  };
}

// =============================================================================
// Emission-day generation
// =============================================================================

/**
 * Normalize an emission day to yyyymmdd format.
 * Returns yesterday's UTC date if not provided or invalid: the auction that takes bids
 * today sells yesterday's emission day.
 */
export function normalizeEmissionDay(emissionDay?: string): string {
  if (emissionDay && /^\d{8}$/.test(emissionDay)) {
    return emissionDay;
  }
  const yesterday = new Date(Date.now() - 86_400_000);
  const y = String(yesterday.getUTCFullYear());
  const m = String(yesterday.getUTCMonth() + 1).padStart(2, "0");
  const d = String(yesterday.getUTCDate()).padStart(2, "0");
  return `${y}${m}${d}`;
}

/**
 * Parse range string in format "min-max" to tuple.
 * Returns undefined if invalid.
 */
export function parseRange(rangeStr?: string): [number, number] | undefined {
  if (!rangeStr || !rangeStr.includes("-")) return undefined;

  const [min, max] = rangeStr.split("-").map((v) => parseFloat(v.trim()));
  if (isNaN(min) || isNaN(max) || min >= max) {
    throw new Error(`Invalid range: ${rangeStr}`);
  }
  return [min, max];
}

/**
 * Convert an emission day (yyyymmdd) to UNIX timestamp at noon UTC.
 */
export function emissionDayToNoonTimestamp(emissionDay: string): bigint {
  const { y, m, d } = yyyymmddToIso(emissionDay);
  const date = new Date(`${y}-${m}-${d}T12:00:00Z`);
  return BigInt(Math.floor(date.getTime() / 1000));
}

/**
 * Convert an emission day string (yyyymmdd) to uint32 number.
 * Example: "20260212" -> 20260212
 */
export function emissionDayToUint32(emissionDay: string): number {
  if (!/^\d{8}$/.test(emissionDay)) {
    throw new Error("emission day must be yyyymmdd, e.g. 20250924");
  }
  return parseInt(emissionDay, 10);
}

/**
 * Convert a uint32 emission day to string format (yyyymmdd).
 * Example: 20260212 -> "20260212"
 */
export function uint32ToEmissionDay(emissionDay: number): string {
  const str = String(emissionDay).padStart(8, "0");
  if (str.length !== 8) {
    throw new Error("emission day must be a valid 8-digit number (yyyymmdd)");
  }
  return str;
}

/**
 * Resolve a uint32 emission day from an explicit yyyymmdd string or fall back to yesterday (UTC).
 * Throws if an explicit string is malformed.
 */
export function resolveEmissionDay(emissionDay?: string): number {
  return emissionDayToUint32(normalizeEmissionDay(emissionDay));
}
