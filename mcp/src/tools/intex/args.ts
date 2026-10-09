import { z } from "zod";
import { toSeriesId } from "../../intex/format.js";

export const accountArg = z.string().optional().describe("0x address to query (default: the configured signer)");
export const seriesArg = z
  .string()
  .describe('series id, e.g. "20260212-TRY-U"')
  .transform((v) => toSeriesId(v));
export const worldwideDayArg = z.number().int().describe("auction worldwide day (yyyymmdd)");
export const bidUnitsArg = z.number().int().min(1).max(65_535).describe("bid units (uint16)");
export const rateArg = z
  .string()
  .describe('bid rate as a fraction of strike, 0..1 (e.g. "0.8" = 80% of strike; min from auction_info)');
export const issuanceCurrencyArg = z
  .number()
  .int()
  .describe("declared issuance currency, ISO 4217 numeric (e.g. 949 = TRY); any 1..999 code");
export const referenceCurrencyArg = z
  .number()
  .int()
  .describe("reference currency the bid prices in, ISO 4217 numeric; must be one the day prices (auction_info)");
export const unitsArg = z.string().describe("Intex units, a whole number");
export const recipientArg = z.string().optional().describe("recipient on outbe (default: the signer)");
export const destinationArg = z
  .string()
  .optional()
  .describe("chain to bridge to: outbe, a network name or a chain id (default outbe, or the one other Intex chain)");
