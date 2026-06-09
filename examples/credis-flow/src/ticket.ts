// Local ticket persistence for the shielded gratis pool demo.
//
// Pledge writes a ticket so unpledge knows the secret + leafIndex; in
// production a wallet would store the same fields. The tickets directory is
// gitignored so test secrets never leave the developer machine.

import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, unlinkSync, writeFileSync } from "fs";
import { dirname, resolve } from "path";
import { fileURLToPath } from "url";

export interface Ticket {
  denomId: number;
  secret: string;          // 0x-prefixed 32-byte hex
  nullifierSecret: string; // 0x-prefixed 32-byte hex
  commitment: string;      // 0x-prefixed 32-byte hex
  leafIndex: number;
  root: string;            // 0x-prefixed 32-byte hex (pool root just after this pledge)
  blockNumber: number;
  txHash: string;
  chainId: string;
  createdAt: string;
}

const TICKETS_DIR = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../tickets",
);

function ensureDir() {
  if (!existsSync(TICKETS_DIR)) mkdirSync(TICKETS_DIR, { recursive: true });
}

function ticketName(t: Pick<Ticket, "denomId" | "commitment">): string {
  const short = t.commitment.replace(/^0x/, "").slice(0, 12);
  return `${t.denomId}-${short}.json`;
}

export function writeTicket(t: Ticket): string {
  ensureDir();
  const path = resolve(TICKETS_DIR, ticketName(t));
  writeFileSync(path, JSON.stringify(t, null, 2) + "\n");
  return path;
}

export function readTicket(path: string): Ticket {
  const raw = readFileSync(path, "utf-8");
  return JSON.parse(raw) as Ticket;
}

export function deleteTicket(path: string): void {
  if (existsSync(path)) unlinkSync(path);
}

// Returns the most recently modified ticket file (optionally filtered by
// denomId). Returns null if the tickets directory is empty.
export function findLatestTicket(denomId?: number): { path: string; ticket: Ticket } | null {
  if (!existsSync(TICKETS_DIR)) return null;
  const entries = readdirSync(TICKETS_DIR)
    .filter((f) => f.endsWith(".json"))
    .map((f) => {
      const path = resolve(TICKETS_DIR, f);
      return { path, mtime: statSync(path).mtimeMs };
    })
    .sort((a, b) => b.mtime - a.mtime);

  for (const { path } of entries) {
    const ticket = readTicket(path);
    if (denomId === undefined || ticket.denomId === denomId) {
      return { path, ticket };
    }
  }
  return null;
}

export { TICKETS_DIR };
