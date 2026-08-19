import { config as dotenvConfig } from 'dotenv';
import { ethers } from 'ethers';

dotenvConfig();

export interface ChainConfig {
  name: string;
  rpc: string;
  chainId: number;
  /** Decimals of the chain's native token: COEN is six-decimal, EVM natives are 18.
   *  Not discoverable over RPC, so it has to be configured per chain. */
  nativeDecimals: number;
}

// Router address (same on all chains)
export const ROUTER = process.env.ROUTER || '0x1619D255A9febB3C66b54Ff17Eb165efbCcda5b3';

// Token addresses
export const INPUT_TOKEN = process.env.INPUT_TOKEN || '0x5cDF01b5Cb3C82a71f423dB6a91c721f138EbEce';
export const OUTPUT_TOKEN = process.env.OUTPUT_TOKEN || '0xe6E008521e1DB2a638863eac4682c2561874F37b';

// Fill deadline (seconds after order creation)
export const FILL_DEADLINE_SECONDS = parseInt(process.env.FILL_DEADLINE_SECONDS || '86400'); // Default: 24 hours

// Number of blocks to query back from current block (for event queries)
// Default: 1999 blocks (safe for most RPC providers that limit to 2000 blocks)
export const QUERY_BLOCKS_BACK = parseInt(process.env.QUERY_BLOCKS_BACK || '1999');

export const chains: Record<string, ChainConfig> = {
  bsc: {
    name: 'BSC Testnet',
    rpc: process.env.BSC_TESTNET_RPC || 'https://bsc-testnet-rpc.publicnode.com',
    chainId: parseInt(process.env.BSC_CHAIN_ID || '97'),
    nativeDecimals: 18,
  },
  sepolia: {
    name: 'Sepolia',
    rpc: process.env.SEPOLIA_RPC || 'https://ethereum-sepolia-rpc.publicnode.com',
    chainId: parseInt(process.env.SEPOLIA_CHAIN_ID || '11155111'),
    nativeDecimals: 18,
  },
  outbe_priv: {
    name: 'Outbe Privnet',
    rpc: process.env.OUTBE_PRIV_RPC || 'https://eth.p.outbe.net',
    chainId: parseInt(process.env.OUTBE_PRIV_CHAIN_ID || '512512'),
    nativeDecimals: 6,
  },

  outbe_dev: {
    name: 'Outbe Devnet',
    rpc: process.env.OUTBE_DEV_RPC || 'https://eth.d.outbe.net',
    chainId: parseInt(process.env.OUTBE_DEV_CHAIN_ID || '424242'),
    nativeDecimals: 6,
  },

  outbe_testnet_old: {
    name: 'Outbe Testnet (old)',
    rpc: process.env.OUTBE_TESTNET_OLD_RPC || 'https://eth.testnet.outbe.net',
    chainId: parseInt(process.env.OUTBE_TESTNET_OLD_CHAIN_ID || '512215'),
    // Predates the six-decimal cutover; confirm before pointing orders at it.
    nativeDecimals: 18,
  },

  outbe_testnet: {
    name: 'Outbe Testnet',
    rpc: process.env.OUTBE_TESTNET_RPC || 'https://rpc.testnet.outbe.net',
    chainId: parseInt(process.env.OUTBE_TESTNET_CHAIN_ID || '54322345'),
    nativeDecimals: 6,
  },
};

/** Native decimals keyed by chain id, for lookups that only have a provider. */
export const nativeDecimalsByChainId: Record<number, number> = Object.fromEntries(
  Object.values(chains).map((chain) => [chain.chainId, chain.nativeDecimals])
);

export const privateKey = process.env.PRIVATE_KEY;

if (!privateKey) {
  throw new Error('PRIVATE_KEY not set in .env file');
}

