// LZ executor re-test: drive a fresh auction, then send the 3 post-clearing
// messages manually and let the executor auto-deliver them (no manual lzReceive).
//   node scripts/intex-test.mjs start   <series>
//   node scripts/intex-test.mjs commit  <series> <qty> <price>
//   node scripts/intex-test.mjs greenday<series>
//   node scripts/intex-test.mjs reveal  <series> <qty> <price>
//   node scripts/intex-test.mjs clear   <series>
//   node scripts/intex-test.mjs sends   <series> <qty> <price>
//   node scripts/intex-test.mjs check   <series>
import { readFileSync } from "node:fs";
import {
  createPublicClient, createWalletClient, defineChain, http,
  encodeFunctionData, keccak256, parseUnits, formatUnits, getAddress, pad, toHex,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";

const A = {
  desis: "0x9fFC07E7Aa63192f3E525586F74cD48754FCA129",      // outbe
  origin: "0x0bDfa41DF2C215dcaF34574f7Bae71845e16F0b0",     // outbe OriginMessenger
  factory: "0x08b166Dd248007424B29650e1a4bdaB12E9BcB2a",    // outbe IntexFactory
  auction: "0x912E4b32b38cc2c4D80047cB476599102c676896",    // bnb IntexAuction
  escrow: "0x47E9e7be5a45B296036b196C0cC8B371A8AB9D83",     // bnb EscrowAdapter
  token: "0x78366397b72D0c283658DA5A38C450455A97e595",      // bnb USDT
  nft: "0xc9735138d43CDc740d8aF43cB6597Ee040Bb1D2d",        // bnb IntexNFT1155
};
const DESIS_ROLE = keccak256(toHex("DESIS_ROLE"));
const BSC_RPC = "https://bsc-testnet.publicnode.com";

// --- ABIs (minimal) ---
const desisAbi = [
  { type: "function", name: "sendAuctionStageStart", stateMutability: "payable", inputs: [{ type: "tuple", name: "config", components: [{ type: "uint32", name: "seriesId" }, { type: "uint32", name: "revealWindow" }, { type: "uint32", name: "issuanceWindow" }, { type: "uint128", name: "intexSize" }, { type: "uint64", name: "minIntexBidPrice" }, { type: "uint64", name: "intexStrikePrice" }, { type: "uint16", name: "minIntexBidQuantity" }] }, { type: "bytes", name: "extraOptions" }], outputs: [] },
  { type: "function", name: "sendAuctionStageReveal", stateMutability: "payable", inputs: [{ type: "uint32" }, { type: "bool" }, { type: "bytes" }], outputs: [] },
  { type: "function", name: "sendAuctionStageClearing", stateMutability: "payable", inputs: [{ type: "uint32" }, { type: "uint256" }, { type: "tuple", components: [{ type: "uint32", name: "intexCallPeriod" }, { type: "uint16", name: "settlementTokenAlias" }, { type: "uint16", name: "callWindowDays" }, { type: "uint16", name: "callThresholdDays" }] }, { type: "bytes" }], outputs: [] },
  { type: "function", name: "getAuctionStage", stateMutability: "view", inputs: [{ type: "uint32" }], outputs: [{ type: "uint8" }] },
];
const auctionAbi = [
  { type: "function", name: "commitBid", stateMutability: "nonpayable", inputs: [{ type: "uint32" }, { type: "bytes32" }], outputs: [] },
  { type: "function", name: "revealBid", stateMutability: "nonpayable", inputs: [{ type: "uint32" }, { type: "uint16" }, { type: "uint64" }, { type: "uint64" }, { type: "bytes" }], outputs: [] },
  { type: "function", name: "getAuctionStage", stateMutability: "view", inputs: [{ type: "uint32" }], outputs: [{ type: "uint8" }] },
];
const erc20Abi = [
  { type: "function", name: "approve", stateMutability: "nonpayable", inputs: [{ type: "address" }, { type: "uint256" }], outputs: [{ type: "bool" }] },
  { type: "function", name: "allowance", stateMutability: "view", inputs: [{ type: "address" }, { type: "address" }], outputs: [{ type: "uint256" }] },
  { type: "function", name: "balanceOf", stateMutability: "view", inputs: [{ type: "address" }], outputs: [{ type: "uint256" }] },
];
const feeT = { type: "tuple", components: [{ type: "uint256", name: "nativeFee" }, { type: "uint256", name: "lzTokenFee" }] };
const originAbi = [
  { type: "function", name: "grantRole", stateMutability: "nonpayable", inputs: [{ type: "bytes32" }, { type: "address" }], outputs: [] },
  { type: "function", name: "revokeRole", stateMutability: "nonpayable", inputs: [{ type: "bytes32" }, { type: "address" }], outputs: [] },
  { type: "function", name: "quoteSendAuctionResult", stateMutability: "view", inputs: [{ type: "uint32" }, { type: "uint32" }, { type: "uint64" }, { type: "uint32" }, { type: "bytes" }, { type: "bool" }], outputs: [feeT] },
  { type: "function", name: "sendAuctionResult", stateMutability: "payable", inputs: [{ type: "uint32" }, { type: "uint32" }, { type: "uint64" }, { type: "uint32" }, { type: "bytes" }, feeT, { type: "address" }], outputs: [{ type: "tuple", components: [{ type: "bytes32" }, { type: "uint64" }, feeT] }] },
  { type: "function", name: "quoteSendRefundInstructions", stateMutability: "view", inputs: [{ type: "uint32" }, { type: "address[]" }, { type: "uint64[]" }, { type: "uint64[]" }, { type: "bytes" }, { type: "bool" }], outputs: [feeT] },
  { type: "function", name: "sendRefundInstructions", stateMutability: "payable", inputs: [{ type: "uint32" }, { type: "address[]" }, { type: "uint64[]" }, { type: "uint64[]" }, { type: "bytes" }, feeT, { type: "address" }], outputs: [{ type: "tuple", components: [{ type: "bytes32" }, { type: "uint64" }, feeT] }] },
];
const factoryAbi = [
  { type: "function", name: "grantRole", stateMutability: "nonpayable", inputs: [{ type: "bytes32" }, { type: "address" }], outputs: [] },
  { type: "function", name: "revokeRole", stateMutability: "nonpayable", inputs: [{ type: "bytes32" }, { type: "address" }], outputs: [] },
  { type: "function", name: "issue", stateMutability: "payable", inputs: [{ type: "tuple", name: "params", components: [{ type: "uint32", name: "seriesId" }, { type: "uint32", name: "issuedIntexCount" }, { type: "uint128", name: "intexSize" }, { type: "uint64", name: "intexStrikePrice" }, { type: "uint64", name: "coenPriceFloor" }, { type: "uint32", name: "intexCallPeriod" }, { type: "uint16", name: "settlementTokenAlias" }, { type: "uint16", name: "callWindowDays" }, { type: "uint16", name: "callThresholdDays" }, { type: "uint64", name: "coenPriceCallTrigger" }, { type: "address[]", name: "recipients" }, { type: "uint256[]", name: "quantities" }] }, { type: "bytes" }], outputs: [] },
];
const nftAbi = [
  { type: "function", name: "getOwnedSeriesWithBalances", stateMutability: "view", inputs: [{ type: "address" }], outputs: [{ type: "uint256[]" }, { type: "uint256[]" }] },
  { type: "function", name: "totalSeries", stateMutability: "view", inputs: [], outputs: [{ type: "uint256" }] },
];

function env() {
  const e = {};
  for (const l of readFileSync(new URL("../.env", import.meta.url), "utf8").split("\n")) {
    const m = l.match(/^\s*([A-Z_]+)\s*=\s*(.*)\s*$/);
    if (m) e[m[1]] = m[2].replace(/^["']|["']$/g, "");
  }
  return e;
}

async function main() {
  const [phase, sArg, qArg, pArg] = process.argv.slice(2);
  const series = Number(sArg);
  const e = env();
  const pk = e.OUTBE_PRIVATE_KEY.startsWith("0x") ? e.OUTBE_PRIVATE_KEY : `0x${e.OUTBE_PRIVATE_KEY}`;
  const account = privateKeyToAccount(pk);
  const me = account.address;

  const outbeT = http(e.OUTBE_RPC_URL);
  const oid = await createPublicClient({ transport: outbeT }).getChainId();
  const outbeChain = defineChain({ id: oid, name: "outbe", nativeCurrency: { name: "COEN", symbol: "COEN", decimals: 18 }, rpcUrls: { default: { http: [e.OUTBE_RPC_URL] } } });
  const outbe = createPublicClient({ chain: outbeChain, transport: outbeT });
  const outbeW = createWalletClient({ account, chain: outbeChain, transport: outbeT });

  const bscChain = defineChain({ id: 97, name: "bsc-testnet", nativeCurrency: { name: "tBNB", symbol: "tBNB", decimals: 18 }, rpcUrls: { default: { http: [BSC_RPC] } } });
  const bsc = createPublicClient({ chain: bscChain, transport: http(BSC_RPC) });
  const bscW = createWalletClient({ account, chain: bscChain, transport: http(BSC_RPC) });

  const mine = (c, h) => c.waitForTransactionReceipt({ hash: h, timeout: 180_000 });
  console.error(`[test] phase=${phase} series=${series} signer=${me}`);

  if (phase === "start") {
    const config = { seriesId: series, revealWindow: 12 * 3600, issuanceWindow: 3600, intexSize: 1000n, minIntexBidPrice: 0n, intexStrikePrice: 2_800_000_000n, minIntexBidQuantity: 4 };
    const h = await outbeW.writeContract({ address: A.desis, abi: desisAbi, functionName: "sendAuctionStageStart", args: [config, "0x"] });
    console.log("start tx", h, "status", (await mine(outbe, h)).status);
  } else if (phase === "commit") {
    const qty = Number(qArg), price = parseUnits(pArg, 6);
    const sig = await signReveal(account, series, me, qty, price);
    const ch = keccak256(sig);
    const h = await bscW.writeContract({ address: A.auction, abi: auctionAbi, functionName: "commitBid", args: [series, ch] });
    console.log("commit tx", h, "status", (await mine(bsc, h)).status, "commitHash", ch);
  } else if (phase === "greenday") {
    const h = await outbeW.writeContract({ address: A.desis, abi: desisAbi, functionName: "sendAuctionStageReveal", args: [series, true, "0x"] });
    console.log("greenday tx", h, "status", (await mine(outbe, h)).status);
  } else if (phase === "reveal") {
    const qty = Number(qArg), price = parseUnits(pArg, 6);
    const lock = BigInt(qty) * price;
    const allowance = await bsc.readContract({ address: A.token, abi: erc20Abi, functionName: "allowance", args: [me, A.escrow] });
    if (allowance < lock) {
      const ah = await bscW.writeContract({ address: A.token, abi: erc20Abi, functionName: "approve", args: [A.escrow, lock] });
      console.log("approve tx", ah, "status", (await mine(bsc, ah)).status, "amount", formatUnits(lock, 6), "USDT");
    }
    const sig = await signReveal(account, series, me, qty, price);
    const h = await bscW.writeContract({ address: A.auction, abi: auctionAbi, functionName: "revealBid", args: [series, qty, price, 97n, sig] });
    console.log("reveal tx", h, "status", (await mine(bsc, h)).status, "locked", formatUnits(lock, 6), "USDT");
  } else if (phase === "clear") {
    const supplyPromis = 100n * 1000n;
    const issuance = { intexCallPeriod: 0, settlementTokenAlias: 840, callWindowDays: 30, callThresholdDays: 20 };
    const h = await outbeW.writeContract({ address: A.desis, abi: desisAbi, functionName: "sendAuctionStageClearing", args: [series, supplyPromis, issuance, "0x"] });
    await mine(outbe, h);
    const stage = Number(await outbe.readContract({ address: A.desis, abi: desisAbi, functionName: "getAuctionStage", args: [series] }));
    console.log("clear tx", h, "DesisStage", stage, "(3=BidsReceived)");
  } else if (phase === "sends") {
    const qty = Number(qArg), price = parseUnits(pArg, 6);
    const clearingPrice = price;                 // 1 bid, undersubscribed → clears at bid price
    const paid = BigInt(qty) * clearingPrice;    // pays for all won
    const floor = (2_800_000_000n * 108n) / (1000n * 100n);
    const callTrig = (floor * 164n) / 100n;
    const issueParams = { seriesId: series, issuedIntexCount: qty, intexSize: 1000n, intexStrikePrice: 2_800_000_000n, coenPriceFloor: floor, intexCallPeriod: 0, settlementTokenAlias: 840, callWindowDays: 30, callThresholdDays: 20, coenPriceCallTrigger: callTrig, recipients: [me], quantities: [BigInt(qty)] };

    console.log("grant DESIS_ROLE on OriginMessenger + IntexFactory");
    await mine(outbe, await outbeW.writeContract({ address: A.origin, abi: originAbi, functionName: "grantRole", args: [DESIS_ROLE, me] }));
    await mine(outbe, await outbeW.writeContract({ address: A.factory, abi: factoryAbi, functionName: "grantRole", args: [DESIS_ROLE, me] }));

    const arFee = await outbe.readContract({ address: A.origin, abi: originAbi, functionName: "quoteSendAuctionResult", args: [series, qty, clearingPrice, 1, "0x", false] });
    const ar = await outbeW.writeContract({ address: A.origin, abi: originAbi, functionName: "sendAuctionResult", args: [series, qty, clearingPrice, 1, "0x", { nativeFee: arFee.nativeFee, lzTokenFee: arFee.lzTokenFee }, me] });
    console.log("1) AuctionResult tx", ar, "status", (await mine(outbe, ar)).status);

    const iss = await outbeW.writeContract({ address: A.factory, abi: factoryAbi, functionName: "issue", args: [issueParams, "0x"] });
    console.log("2) issue tx", iss, "status", (await mine(outbe, iss)).status);

    const rfFee = await outbe.readContract({ address: A.origin, abi: originAbi, functionName: "quoteSendRefundInstructions", args: [series, [me], [0n], [paid], "0x", false] });
    const rf = await outbeW.writeContract({ address: A.origin, abi: originAbi, functionName: "sendRefundInstructions", args: [series, [me], [0n], [paid], "0x", { nativeFee: rfFee.nativeFee, lzTokenFee: rfFee.lzTokenFee }, me] });
    console.log("3) Refund tx", rf, "status", (await mine(outbe, rf)).status);

    await mine(outbe, await outbeW.writeContract({ address: A.origin, abi: originAbi, functionName: "revokeRole", args: [DESIS_ROLE, me] }));
    await mine(outbe, await outbeW.writeContract({ address: A.factory, abi: factoryAbi, functionName: "revokeRole", args: [DESIS_ROLE, me] }));
    console.log("roles revoked. 3 messages sent — NOW WAITING ON THE EXECUTOR (no manual lzReceive).");
  } else if (phase === "check") {
    const bnbTotal = await bsc.readContract({ address: A.nft, abi: nftAbi, functionName: "totalSeries" });
    const [ids, bals] = await bsc.readContract({ address: A.nft, abi: nftAbi, functionName: "getOwnedSeriesWithBalances", args: [me] });
    console.log("BNB NFT totalSeries:", bnbTotal.toString(), "| bidder tokens:", ids.length, "balances:", bals.map((b) => b.toString()).join(","));
  } else {
    throw new Error("unknown phase");
  }
}

async function signReveal(account, series, bidder, quantity, bidPrice) {
  return account.signTypedData({
    domain: { name: "IntexAuction", version: "1", chainId: 97, verifyingContract: getAddress(A.auction) },
    types: { RevealBid: [{ name: "seriesId", type: "uint32" }, { name: "bidder", type: "address" }, { name: "quantity", type: "uint16" }, { name: "bidPrice", type: "uint64" }] },
    primaryType: "RevealBid",
    message: { seriesId: series, bidder, quantity, bidPrice },
  });
}

main().catch((err) => { console.error("[test] error:", err.shortMessage ?? err.message ?? err); process.exit(1); });
