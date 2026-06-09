import { ethers, JsonRpcProvider } from 'ethers';
import { chains, privateKey, ROUTER } from '../config';
import { LayerZeroRouter__factory } from '../typechain';

const LZ_ENDPOINT_ABI = [
  'function inboundNonce(address _receiver, uint32 _srcEid, bytes32 _sender) view returns (uint64)',
  'function lazyInboundNonce(address _receiver, uint32 _srcEid, bytes32 _sender) view returns (uint64)',
  'function outboundNonce(address _sender, uint32 _dstEid, bytes32 _receiver) view returns (uint64)',
  'function inboundPayloadHash(address _receiver, uint32 _srcEid, bytes32 _sender, uint64 _nonce) view returns (bytes32)',
  'function skip(address _oapp, uint32 _srcEid, bytes32 _sender, uint64 _nonce)',
  'function delegates(address _oapp) view returns (address)',
];

/**
 * Clear stuck LayerZero nonces between two chains.
 * Skips all unverified nonces in the gap between inboundNonce and outboundNonce.
 * Verified-but-unexecuted nonces are reported (need manual clear with guid+message).
 *
 * Usage: tsx scripts/clear_nonces.ts <destChain> <srcChain>
 *
 * Example: tsx scripts/clear_nonces.ts outbe_testnet bsc
 *   → clears stuck nonces on outbe_testnet coming from bsc
 */
async function main() {
  console.log('LayerZero - Clear Stuck Nonces\n');

  const [destChainName, srcChainName] = process.argv.slice(2);
  if (!destChainName || !srcChainName) {
    console.error('Usage: tsx scripts/clear_nonces.ts <destChain> <srcChain>');
    process.exit(1);
  }
  if (!chains[destChainName]) {
    console.error('Invalid dest chain. Available:', Object.keys(chains).join(', '));
    process.exit(1);
  }
  if (!chains[srcChainName]) {
    console.error('Invalid src chain. Available:', Object.keys(chains).join(', '));
    process.exit(1);
  }

  const destProvider = new JsonRpcProvider(chains[destChainName].rpc);
  const srcProvider = new JsonRpcProvider(chains[srcChainName].rpc);
  const wallet = new ethers.Wallet(privateKey!, destProvider);

  const destRouter = LayerZeroRouter__factory.connect(ROUTER, destProvider);
  const srcRouter = LayerZeroRouter__factory.connect(ROUTER, srcProvider);

  // Get LZ endpoint and EIDs
  const endpointAddr = await destRouter.endpoint();
  const endpoint = new ethers.Contract(endpointAddr, LZ_ENDPOINT_ABI, wallet);

  const srcEndpointAddr = await srcRouter.endpoint();
  const srcEndpoint = new ethers.Contract(srcEndpointAddr, LZ_ENDPOINT_ABI, srcProvider);

  // Get EIDs from endpoints
  const srcEid: number = Number(await srcEndpoint.getFunction('inboundNonce').staticCall(ROUTER, 0, ethers.ZeroHash).catch(() => null)
    // Fallback: read eid() from endpoint
    || 0);

  // Actually get EID by checking what outbound nonce exists
  // We need srcEid — read it from the src endpoint
  const srcEidContract = new ethers.Contract(srcEndpointAddr, ['function eid() view returns (uint32)'], srcProvider);
  const destEidContract = new ethers.Contract(endpointAddr, ['function eid() view returns (uint32)'], destProvider);

  const srcEidVal = Number(await srcEidContract.eid());
  const destEidVal = Number(await destEidContract.eid());

  console.log(`  Dest chain:  ${destChainName} (EID: ${destEidVal})`);
  console.log(`  Src chain:   ${srcChainName} (EID: ${srcEidVal})`);

  const routerB32 = ethers.zeroPadValue(ROUTER, 32);

  // Check nonces
  const outbound = Number(await srcEndpoint.outboundNonce(ROUTER, destEidVal, routerB32));
  const inbound = Number(await endpoint.inboundNonce(ROUTER, srcEidVal, routerB32));
  const lazy = Number(await endpoint.lazyInboundNonce(ROUTER, srcEidVal, routerB32));

  console.log(`\n  Outbound (${srcChainName} → ${destChainName}): ${outbound}`);
  console.log(`  Inbound  (${destChainName} ← ${srcChainName}): ${inbound}`);
  console.log(`  Lazy     (${destChainName} ← ${srcChainName}): ${lazy}`);

  if (inbound >= outbound) {
    console.log('\n  All nonces are synced. Nothing to clear.');
    return;
  }

  const gap = outbound - inbound;
  console.log(`\n  Gap: ${gap} stuck nonce(s) [${inbound + 1}..${outbound}]`);

  // Check delegate
  const delegate = await endpoint.delegates(ROUTER);
  const walletAddr = await wallet.getAddress();
  if (delegate.toLowerCase() !== walletAddr.toLowerCase()) {
    console.error(`\n  Your wallet (${walletAddr}) is not the delegate (${delegate}).`);
    console.error('  Cannot skip/clear nonces.');
    process.exit(1);
  }

  let skipped = 0;
  let verified = 0;

  for (let nonce = inbound + 1; nonce <= outbound; nonce++) {
    const payloadHash = await endpoint.inboundPayloadHash(ROUTER, srcEidVal, routerB32, nonce);
    const isVerified = payloadHash !== ethers.ZeroHash;

    if (isVerified) {
      console.log(`  Nonce ${nonce}: verified — needs manual clear (guid+message required)`);
      verified++;
    } else {
      process.stdout.write(`  Nonce ${nonce}: skipping...`);
      const tx = await endpoint.skip(ROUTER, srcEidVal, routerB32, nonce);
      await tx.wait();
      console.log(` done (tx: ${tx.hash})`);
      skipped++;
    }
  }

  // Final state
  const finalInbound = Number(await endpoint.inboundNonce(ROUTER, srcEidVal, routerB32));
  const finalLazy = Number(await endpoint.lazyInboundNonce(ROUTER, srcEidVal, routerB32));

  console.log(`\nResult:`);
  console.log(`  Skipped: ${skipped}`);
  console.log(`  Verified (manual): ${verified}`);
  console.log(`  Inbound nonce: ${inbound} → ${finalInbound}`);
  console.log(`  Lazy nonce: ${lazy} → ${finalLazy}`);
}

main()
  .then(() => process.exit(0))
  .catch((error) => { console.error('Error:', error.message); process.exit(1); });
