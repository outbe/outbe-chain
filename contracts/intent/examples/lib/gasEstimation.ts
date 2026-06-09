/**
 * Gas estimation utilities
 */

import { ethers } from 'ethers';
import type { LayerZeroRouter } from '../typechain';

/**
 * Estimates gas for a contract call and adds a buffer
 * @param estimateGasFn - Function that estimates gas (e.g., contract.method.estimateGas)
 * @param bufferPercent - Buffer percentage to add (default: 20%)
 * @returns Gas limit with buffer applied
 */
export async function estimateGasWithBuffer(
  estimateGasFn: () => Promise<bigint>,
  bufferPercent: number = 20
): Promise<bigint> {
  try {
    const estimatedGas = await estimateGasFn();
    console.log(`  Estimated gas: ${estimatedGas.toString()}`);

    // Add buffer to estimated gas
    const gasLimit = (estimatedGas * BigInt(100 + bufferPercent)) / 100n;
    console.log(`  Gas limit: ${gasLimit.toString()}`);

    return gasLimit;
  } catch (error: any) {
    console.error('  ❌ Gas estimation failed:', error.message);
    if (error.data) {
      console.error('  Error data:', error.data);
    }
    throw error;
  }
}

/**
 * Calculates LayerZero messaging fee for refund operation
 * @param router - LayerZeroRouter contract instance
 * @param originChainId - Origin chain domain ID
 * @param orderIds - Array of order IDs to refund
 * @returns MessagingFee with nativeFee and lzTokenFee
 */
export async function calculateRefundFee(
  router: LayerZeroRouter,
  originChainId: number,
  orderIds: string[]
): Promise<{ nativeFee: bigint; lzTokenFee: bigint }> {
  // Create payload for quote (same as LayerZeroRouterMessage.encodeRefund)
  const payload = ethers.AbiCoder.defaultAbiCoder().encode(
    ['bool', 'bytes32[]', 'bytes[]'],
    [false, orderIds, []]
  );

  // Get LayerZero messaging fee
  return await router.quote(originChainId, payload, false);
}

/**
 * Calculates LayerZero messaging fee for settle operation
 * @param router - LayerZeroRouter contract instance
 * @param originChainId - Origin chain domain ID
 * @param orderIds - Array of order IDs to settle
 * @param ordersFillerData - Array of filler data for each order
 * @returns MessagingFee with nativeFee and lzTokenFee
 */
export async function calculateSettleFee(
  router: LayerZeroRouter,
  originChainId: number,
  orderIds: string[],
  ordersFillerData: string[]
): Promise<{ nativeFee: bigint; lzTokenFee: bigint }> {
  // Create payload for quote (same as LayerZeroRouterMessage.encodeSettle)
  const payload = ethers.AbiCoder.defaultAbiCoder().encode(
    ['bool', 'bytes32[]', 'bytes[]'],
    [true, orderIds, ordersFillerData]
  );

  // Get LayerZero messaging fee
  return await router.quote(originChainId, payload, false);
}
