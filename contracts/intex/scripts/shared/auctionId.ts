// Series ID Utilities
// Functions for working with series identifiers.
//
// Conventions
// - seriesId: 8-digit numeric in yyyymmdd format (uint32 on-chain), e.g. 20250924.
//   A plain TypeScript `number` is the sole key for every auction and series.

// =============================================================================
// Parsing
// =============================================================================

/** Parse yyyymmdd format to ISO components */
export function yyyymmddToIso(seriesId: string): { y: string; m: string; d: string } {
  if (!/^\d{8}$/.test(seriesId)) {
    throw new Error("series must be yyyymmdd, e.g. 20250924");
  }
  return {
    y: seriesId.slice(0, 4),
    m: seriesId.slice(4, 6),
    d: seriesId.slice(6, 8),
  };
}

// =============================================================================
// Series ID Generation
// =============================================================================

/**
 * Normalize series ID to yyyymmdd format.
 * Returns today's date if not provided or invalid.
 */
export function normalizeSeries(series?: string): string {
  if (series && /^\d{8}$/.test(series)) {
    return series;
  }
  const now = new Date();
  const y = String(now.getFullYear());
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
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
 * Convert seriesId (yyyymmdd) to UNIX timestamp at noon UTC.
 */
export function seriesIdToNoonTimestamp(seriesId: string): bigint {
  const { y, m, d } = yyyymmddToIso(seriesId);
  const date = new Date(`${y}-${m}-${d}T12:00:00Z`);
  return BigInt(Math.floor(date.getTime() / 1000));
}

/**
 * Convert seriesId string (yyyymmdd) to uint32 number.
 * Example: "20260212" -> 20260212
 */
export function seriesIdToUint32(seriesId: string): number {
  if (!/^\d{8}$/.test(seriesId)) {
    throw new Error("series must be yyyymmdd, e.g. 20250924");
  }
  return parseInt(seriesId, 10);
}

/**
 * Convert uint32 seriesId to string format (yyyymmdd).
 * Example: 20260212 -> "20260212"
 */
export function uint32ToSeriesId(seriesId: number): string {
  const str = String(seriesId).padStart(8, "0");
  if (str.length !== 8) {
    throw new Error("seriesId must be a valid 8-digit number (yyyymmdd)");
  }
  return str;
}

/**
 * Resolve a uint32 seriesId from an explicit series string or fall back to today.
 * Throws if an explicit series string is malformed.
 */
export function resolveSeriesId(series?: string): number {
  return seriesIdToUint32(normalizeSeries(series));
}
