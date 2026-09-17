// Public position receipts only. Private notes stay in the owner wallet.
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, unlinkSync, writeFileSync } from "fs";
import { dirname, resolve } from "path";
import { fileURLToPath } from "url";

export interface Ticket {
  positionId: string;
  smartAccount: string;
  chainId: string;
  txHash: string;
  createdAt: string;
}

const TICKETS_DIR = resolve(dirname(fileURLToPath(import.meta.url)), "../tickets");

function ensureDir() {
  if (!existsSync(TICKETS_DIR)) mkdirSync(TICKETS_DIR, { recursive: true });
}

function ticketName(t: Ticket): string {
  return `credis-${t.chainId}-${t.positionId}.json`;
}

export function writeTicket(t: Ticket): string {
  ensureDir();
  const path = resolve(TICKETS_DIR, ticketName(t));
  writeFileSync(path, JSON.stringify(t, null, 2) + "\n", {mode: 0o600, flag: "wx"});
  return path;
}

export function readTicket(path: string): Ticket {
  return JSON.parse(readFileSync(path, "utf-8")) as Ticket;
}

export function deleteTicket(path: string): void {
  if (existsSync(path)) unlinkSync(path);
}

/** All ticket files, newest first. */
export function listTickets(): { path: string; ticket: Ticket }[] {
  if (!existsSync(TICKETS_DIR)) return [];
  return readdirSync(TICKETS_DIR)
    .filter((f) => f.endsWith(".json"))
    .map((f) => resolve(TICKETS_DIR, f))
    .map((path) => ({ path, ticket: readTicket(path), mtime: statSync(path).mtimeMs }))
    .sort((a, b) => b.mtime - a.mtime)
    .map(({ path, ticket }) => ({ path, ticket }));
}

/** Most recently modified ticket file, or null if the directory is empty. */
export function findLatestTicket(): { path: string; ticket: Ticket } | null {
  return listTickets()[0] ?? null;
}

export { TICKETS_DIR };
