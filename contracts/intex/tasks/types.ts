// Task Types
// Type definitions for Hardhat task arguments.

import type { TaskArgs } from "../scripts/shared/types.js";

// =============================================================================
// Common Arguments
// =============================================================================

export interface CommonAuctionTaskArgs extends TaskArgs {
  intexAuctionContract?: string;
  series?: string;
}

// =============================================================================
// Auction Flow Tasks
// =============================================================================

export interface AuctionFlowTaskArgs extends CommonAuctionTaskArgs {
  floor?: string;
  commitEnd?: string;
  revealEnd?: string;
  issuanceEnd?: string;
  intexSize?: string;
  intexStrikePrice?: string;
  coenPriceFloor?: string;
  minIntexBidQuantity?: string;
  isGreenDay?: string | boolean;
  issuedIntexCount?: string;
  clearingPrice?: string;
  wonBidsCount?: string;
  skipStart?: boolean;
  skipReveal?: boolean;
  skipClearing?: boolean;
  skipExecute?: boolean;
  finalizeEscrow?: string | boolean;
  escrowAdapterContract?: string;
  paymentTokenContract?: string;
}

export interface AuctionStartTaskArgs extends CommonAuctionTaskArgs {
  floor?: string;
  commitEnd?: string;
  revealEnd?: string;
  issuanceEnd?: string;
  intexSize?: string;
  intexStrikePrice?: string;
  coenPriceFloor?: string;
  minIntexBidQuantity?: string;
}

export interface AuctionRevealTaskArgs extends CommonAuctionTaskArgs {
  isGreenDay?: string | boolean;
}

export interface AuctionClearingTaskArgs extends CommonAuctionTaskArgs {
  // No additional fields
}

export interface AuctionExecuteTaskArgs extends CommonAuctionTaskArgs {
  issuedIntexCount?: string;
  clearingPrice?: string;
  wonBidsCount?: string;
  finalizeEscrow?: string | boolean;
  escrowAdapterContract?: string;
  paymentTokenContract?: string;
}

// =============================================================================
// Bidder Tasks
// =============================================================================

export interface BiddersCommitTaskArgs extends CommonAuctionTaskArgs {
  wallets?: string;
  qtyRange?: string;
  priceRange?: string;
  commitsFile?: string;
}

export interface BiddersRevealTaskArgs extends CommonAuctionTaskArgs {
  wallets?: string;
  commitsFile?: string;
  escrowAdapterContract?: string;
  paymentTokenContract?: string;
}

// =============================================================================
// Interactive Task
// =============================================================================

export interface AuctionInteractiveTaskArgs extends CommonAuctionTaskArgs {
  floor?: string;
  commitEnd?: string;
  revealEnd?: string;
  issuanceEnd?: string;
  intexSize?: string;
  intexStrikePrice?: string;
  coenPriceFloor?: string;
  minIntexBidQuantity?: string;
  isGreenDay?: string | boolean;
  wallets?: string;
  commitsFile?: string;
  issuedIntexCount?: string;
  clearingPrice?: string;
  wonBidsCount?: string;
  finalizeEscrow?: string | boolean;
  escrowAdapterContract?: string;
  paymentTokenContract?: string;
  intexContract?: string;
}

// =============================================================================
// Issuance Tasks
// =============================================================================

export interface Intex1155IssuanceTaskArgs extends TaskArgs {
  series?: string;
  allocations?: string;
  intexAuctionContract?: string;
  intexContract?: string;
  skipCreate?: boolean;
  skipMint?: boolean;
}

// =============================================================================
// Settlement Tasks
// =============================================================================

export interface SettlementInteractiveTaskArgs extends TaskArgs {
  desisContract?: string;
  intexBscContract?: string;
  intexOutbeContract?: string;
  targetMessengerContract?: string;
  series?: string;
  bscNetwork?: string;
  msgValue?: string;
  skipFund?: string | boolean;
}

export interface SettlementSettleTaskArgs extends TaskArgs {
  settlementContract?: string;
  intexContract?: string;
  promisContract?: string;
  series?: string;
  intexHolder?: string;
}

export interface SettlementFullFlowTaskArgs extends SettlementInteractiveTaskArgs {
  settlementContract?: string;
  promisContract?: string;
  intexHolder?: string;
}

// =============================================================================
// Outbe Mock Tasks
// =============================================================================

export interface OutbeMockInteractiveTaskArgs extends TaskArgs {
  desisContract?: string;
  promisLimitContract?: string;
  series?: string;
  floor?: string;
  clearingTimestamp?: string;
  intexSize?: string;
  intexStrikePrice?: string;
  minIntexBidQuantity?: string;
  supply?: string;
  msgValue?: string;
  isGreenDay?: string | boolean;
  withBidders?: string | boolean;
  wallets?: string;
  commitsFile?: string;
  bnbAuctionContract?: string;
  bnbEscrowAdapterContract?: string;
  bnbPaymentTokenContract?: string;
  targetMessengerContract?: string;
  bnbBiddersNetwork?: string;
  quantityRange?: string;
  priceRange?: string;
}
