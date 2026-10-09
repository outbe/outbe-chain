import { type Hex, encodeAbiParameters, parseAbiParameters } from "viem";

export interface Phase {
  phase: string;
  next: string;
}

interface Statuses {
  origin: string;
  destination: string;
  expired: boolean;
}

/** Checked in order: the first match names the phase. */
const PHASES: (Phase & { when: (s: Statuses) => boolean })[] = [
  { when: (s) => s.origin === "SETTLED", phase: "SETTLED", next: "done - solver paid on origin" },
  { when: (s) => s.origin === "REFUNDED", phase: "REFUNDED", next: "done - input returned to user" },
  { when: (s) => s.destination === "FILLED", phase: "FILLED", next: "awaiting settle message to origin -> SETTLED" },
  { when: (s) => s.destination === "CLAIMED", phase: "CLAIMED", next: "winner is filling on destination" },
  { when: (s) => s.origin === "OPENED" && s.expired, phase: "EXPIRED", next: "refundable via intent_order_refund" },
  {
    when: (s) => s.origin === "OPENED",
    phase: "OPENED",
    next: "auction running on destination - waiting for a solver to claim & fill",
  },
];

/** The coarse lifecycle phase both routers' statuses put an order in, and what comes next. */
export function derivePhase(origin: string, destination: string, expired: boolean): Phase {
  const match = PHASES.find((p) => p.when({ origin, destination, expired }));
  return match ? { phase: match.phase, next: match.next } : { phase: origin, next: "-" };
}

/** The router's refund message: `(bool false, bytes32[] ids, bytes[] [])`. */
export function refundPayload(orderId: Hex): Hex {
  return encodeAbiParameters(parseAbiParameters("bool, bytes32[], bytes[]"), [false, [orderId], []]);
}
