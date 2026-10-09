// Auction ids are worldwide days (yyyymmdd), one per day. The auction runs weeks
// after its day, so active ids sit up to ~26 days in the past. Discovery probes
// getAuctionStage across a date window - a few cheap point reads - rather than
// scanning logs, which public RPCs range-limit.
export const DEFAULT_DAYS_BACK = 30;
export const DEFAULT_DAYS_AHEAD = 2;
const DAY_MS = 86_400_000;

function ymdToDate(ymd: number): Date {
  return new Date(Date.UTC(Math.floor(ymd / 10000), (Math.floor(ymd / 100) % 100) - 1, ymd % 100));
}
function dateToYmd(dt: Date): number {
  return dt.getUTCFullYear() * 10000 + (dt.getUTCMonth() + 1) * 100 + dt.getUTCDate();
}
export function todayYmd(): number {
  return dateToYmd(new Date());
}
export function ymdShift(ymd: number, days: number): number {
  return dateToYmd(new Date(ymdToDate(ymd).getTime() + days * DAY_MS));
}
export function ymdRange(from: number, to: number): number[] {
  const out: number[] = [];
  for (const dt = ymdToDate(from); dateToYmd(dt) <= to; dt.setUTCDate(dt.getUTCDate() + 1)) out.push(dateToYmd(dt));
  return out;
}
