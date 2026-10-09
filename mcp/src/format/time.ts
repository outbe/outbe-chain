/** A Unix timestamp with its ISO form, or null when it is unset (zero). */
export function epochIso(v: number | bigint): { epoch: number; iso: string } | null {
  const sec = Number(v);
  if (!Number.isFinite(sec) || sec <= 0) return null;
  return { epoch: sec, iso: new Date(sec * 1000).toISOString() };
}
