export const DEFAULT_RPC = "https://rpc.testnet.outbe.net";

export interface Config {
  rpcUrl: string;
  privateKey?: string;
  intentRouter?: string;
  intexOriginRouter?: string;
  /** RPC URLs by chain id, from `OUTBE_RPC_<chainId>`. */
  chainRpcs: Record<number, string>;
}

/** Server settings from `--rpc` and the `OUTBE_*` environment. */
export function loadConfig(argv: readonly string[] = [], env: NodeJS.ProcessEnv = process.env): Config {
  const flag = argv.indexOf("--rpc");
  return {
    rpcUrl: (flag >= 0 && argv[flag + 1]) || (env.OUTBE_RPC ?? DEFAULT_RPC),
    privateKey: env.OUTBE_PRIVATE_KEY,
    intentRouter: env.OUTBE_INTENT_ROUTER,
    intexOriginRouter: env.OUTBE_INTEX_ORIGIN_ROUTER,
    chainRpcs: Object.fromEntries(
      Object.entries(env).flatMap(([name, url]) => {
        const id = /^OUTBE_RPC_(\d+)$/.exec(name)?.[1];
        return id && url ? [[Number(id), url]] : [];
      }),
    ),
  };
}
