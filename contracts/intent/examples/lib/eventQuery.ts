import type { Provider, ContractEventName, BaseContract } from 'ethers';

/**
 * Event query utilities
 */

/**
 * Queries contract events with automatic chunking to avoid RPC limits
 * @param contract - Contract instance
 * @param filter - Event filter
 * @param fromBlock - Starting block number
 * @param toBlock - Ending block number or 'latest'
 * @param provider - Provider instance
 * @param chunkSize - Maximum blocks per query (default: 50000)
 * @returns Array of events
 */
export type EventQueryOptions<T extends BaseContract> = {
  contract: T;
  filter: any;
  fromBlock: number;
  toBlock: number | 'latest';
  provider: Provider;
  chunkSize?: number;
};

export async function queryEventsWithChunking<T extends BaseContract>(
  ...args: [options: EventQueryOptions<T>] | [
    contract: T,
    filter: any,
    fromBlock: number,
    toBlock: number | 'latest',
    provider: Provider,
    chunkSize?: number,
  ]
): Promise<any[]> {
  const options = args.length === 1 ? args[0] : {
    contract: args[0], filter: args[1], fromBlock: args[2],
    toBlock: args[3], provider: args[4], chunkSize: args[5],
  };
  const { contract, filter, fromBlock, toBlock, provider, chunkSize = 50000 } = options;
  const currentBlock = toBlock === 'latest' ? await provider.getBlockNumber() : toBlock;

  let events: any[] = [];
  let startBlock = fromBlock;

  while (startBlock <= currentBlock) {
    const endBlock = Math.min(startBlock + chunkSize - 1, currentBlock);
    console.log(`  Querying blocks ${startBlock} to ${endBlock}...`);

    const chunkEvents = await contract.queryFilter(filter, startBlock, endBlock);
    events = events.concat(chunkEvents);

    startBlock = endBlock + 1;
  }

  return events;
}
