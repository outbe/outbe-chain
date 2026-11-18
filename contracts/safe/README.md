# Safe

Install dependencies and deploy the Safe infrastructure:

```bash
cp .env.example .env
mise run install
mise run deploy
```

Copy `SAFE_SINGLETON`, `SAFE_PROXY_FACTORY` and `SAFE_FALLBACK_HANDLER` from the output to `.env`, set `SAFE_OWNERS` and `SAFE_THRESHOLD`, then create a wallet:

```bash
mise run create-wallet
```

`DEPLOYER_PK` can belong to any funded account when running `create-wallet`.
