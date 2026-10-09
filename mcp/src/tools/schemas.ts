import { z } from "zod";
import { NETWORKS } from "../net/chains.js";

export const address = z.string().describe("0x-prefixed address");

export const networkName = z.string().describe(`network name (one of: ${NETWORKS.map((d) => d.name).join(", ")})`);

export const waitFlag = z.boolean().optional().describe("wait for the receipt (default true)");
