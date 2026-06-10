/**
 * Decoders for Intex enum codes into human-readable names, plus small shaping
 * helpers for tool output. Enum orderings are verbatim from the contracts:
 *  - AuctionStage ... contracts/intex/contracts/bnb/interfaces/IIntexAuction.sol
 *  - IntexState / IntexStatus ... contracts/intex/contracts/shared/interfaces/IIntexNFT1155.sol
 */

const AUCTION_STAGE = ["CommittingBids", "RevealingBids", "Issuance", "Completed", "Cancelled"];
const INTEX_STATE = ["Issued", "Qualified", "Called"];
const INTEX_STATUS = ["Issued", "Settled"];

function label(table: string[], code: number | bigint): { code: number; name: string } {
  const c = Number(code);
  return { code: c, name: table[c] ?? `unknown(${c})` };
}

export const auctionStage = (code: number | bigint) => label(AUCTION_STAGE, code);
export const intexState = (code: number | bigint) => label(INTEX_STATE, code);
export const intexStatus = (code: number | bigint) => label(INTEX_STATUS, code);

/** Auction stages a participant can still act in (commit or reveal). */
export function isActiveStage(code: number | bigint): boolean {
  const c = Number(code);
  return c === 0 || c === 1; // CommittingBids | RevealingBids
}

/** A unix-seconds u32 as { epoch, iso }, or null when zero/unset. */
export function epochIso(v: number | bigint): { epoch: number; iso: string } | null {
  const sec = Number(v);
  if (!Number.isFinite(sec) || sec <= 0) return null;
  return { epoch: sec, iso: new Date(sec * 1000).toISOString() };
}
