import { z } from "zod";
import { NETWORK_NAMES } from "../net/chains.js";

export const address = z.string().describe("0x-prefixed address");

export const networkName = z
  .string()
  .describe(`network: ${NETWORK_NAMES.join(", ")} or a chain id (outbe is the connected node)`);

export const waitFlag = z.boolean().optional().describe("wait for the receipt (default true)");

export const coenAmount = z.string().describe('amount in whole COEN, e.g. "100" or "1.5"');

/** `0x`-hex of exactly 32 bytes - the enclave parses these fields as fixed-width. */
export const HEX32 = /^0x(?:[0-9a-fA-F]{2}){32}$/;
