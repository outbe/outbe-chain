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

// Short clarifications for non-obvious auction stages. The name stays exactly as
// the contract enum; the note only explains it.
const AUCTION_STAGE_NOTE: Record<number, string> = {
  2: "reveal window ended, awaiting clearing", // Issuance
};

export const auctionStage = (code: number | bigint) => {
  const base = label(AUCTION_STAGE, code);
  const note = AUCTION_STAGE_NOTE[base.code];
  return note ? { ...base, note } : base;
};
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
