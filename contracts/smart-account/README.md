# Smart Account Solution

## Architecture Diagram

```mermaid
flowchart TB
    User((User))
    CCA((CCA))
    Bundler[Bundler]

    subgraph Onchain
        EntryPoint[EntryPoint v0.7]

        subgraph KernelAccount[Kernel Smart Account]
            RootValidator[Root Validator<br/>ECDSAValidator]
            RootHook[Root Hook]
            CCAPermission[CCA Permission<br/>per bundle token]
        end

        subgraph OutbeModules[Outbe Kernel Modules]
            BundlePlugin[BundleModulePlugin<br/>executor + fallback]
            SpendHook[BundleSpendProtectorHook<br/>root hook]
            WithdrawHook[BundleWithdrawHook<br/>per-permission hook]
            LimitPolicy[WithdrawalLimitPolicy<br/>per-permission policy]
        end

        Vault[Vault<br/>ERC-20 liquidity]
    end

    User -->|signs UserOp| Bundler
    CCA -->|signs UserOp| Bundler
    Bundler -->|handleOps| EntryPoint
    EntryPoint -->|validateUserOp / execute| KernelAccount

    CCAPermission -->|enforces limit| LimitPolicy
    CCAPermission -->|tracks withdrawal| WithdrawHook
    WithdrawHook -.->|decrements balance| BundlePlugin
    Vault -->|topUp ERC-20| KernelAccount
    BundlePlugin -->|executeFromExecutor<br/>transfer tokens| KernelAccount

    style User fill:#E8F5E9,stroke:#2E7D32,stroke-width:2px,color:#1B5E20
    style CCA fill:#FFF3E0,stroke:#E65100,stroke-width:2px,color:#BF360C
    style EntryPoint fill:#E3F2FD,stroke:#1565C0,stroke-width:2px,color:#0D47A1
    style KernelAccount fill:#F3E5F5,stroke:#7B1FA2,color:#4A148C
    style RootValidator fill:#F3E5F5,stroke:#7B1FA2,color:#4A148C
    style RootHook fill:#F3E5F5,stroke:#7B1FA2,color:#4A148C
    style CCAPermission fill:#F3E5F5,stroke:#7B1FA2,color:#4A148C
    style OutbeModules fill:#FFF8E1,stroke:#F57F17,color:#E65100
    style BundlePlugin fill:#FFF8E1,stroke:#F57F17,color:#E65100
    style SpendHook fill:#FFF8E1,stroke:#F57F17,color:#E65100
    style WithdrawHook fill:#FFF8E1,stroke:#F57F17,color:#E65100
    style LimitPolicy fill:#FFF8E1,stroke:#F57F17,color:#E65100
    style Vault fill:#ECEFF1,stroke:#455A64,stroke-width:2px,color:#263238
    style Bundler fill:#ECEFF1,stroke:#455A64,stroke-width:2px,color:#263238
```

## Actors

### User
Owner of the smart account. Only restricted by BundleSpendProtectorHook prevents spending tokens reserved in bundles — only the free balance (total minus bundled) is available.

### CCA
Authorized party that can withdraw bundle tokens on behalf of the user within configured limits. Each CCA permission is scoped to a specific token with a rolling-window spending cap.

## Onchain Components

### EntryPoint
Standard ERC-4337 v0.7 contract. Receives packed user operations from the bundler, validates them against the smart account, and executes the resulting calls.

### Kernel Account
ZeroDev Kernel v3.1 modular smart account. Supports ERC-7579 modules (validators, executors, hooks, fallbacks, policies). The account is initialized via `SmartAccountFactory` with all Outbe modules pre-configured.

### Outbe Kernel Modules

#### BundleModulePlugin
Singleton ERC-7579 module installed as both **executor** and **fallback**. Manages per-token bundle balances within each smart account.

- `topUp` — Vault calls this fallback to transfer ERC-20 tokens into the account and increase the bundle balance.
- `withdraw` — Decrements bundle balance when tokens are withdrawn by CCA.
- `balanceOf` / `isBundleToken` — Queried by hooks to determine reserved amounts.

#### BundleSpendProtectorHook
Root execution hook applied to the **User's** validator. Intercepts every outgoing ERC-20 `transfer` and reverts if the amount exceeds the free balance (`totalBalance - bundleBalance`). Prevents the user from spending tokens reserved in bundles.

#### BundleWithdrawHook
Per-permission execution hook applied to **CCA** permissions. Validates that the CCA is transferring a registered bundle token, then decrements the bundle balance in `postCheck` via the plugin's executor dispatch.

#### WithdrawalLimitPolicy
ERC-7579 policy (module type 5) applied to **CCA** permissions. Enforces a cumulative spending limit over a rolling time window (e.g. 1000 USDC per day). Resets the used amount when the window expires. Returns `ValidUntil` set to the window end so the EntryPoint can enforce time-bounded validity.

### Vault
Liquidity source that holds ERC-20 tokens. Calls `BundleModulePlugin.topUp` to fund the smart account's bundle balance.

## Offchain Components

### Bundler
ERC-4337 and ERC-7579 compatible service. Receives signed user operations from Users or CCAs, bundles them, and submits them to the EntryPoint for on-chain execution.
1. Accepts UserOperations via a JSON-RPC endpoint (eth_sendUserOperation)
2. Validates them — simulates execution, checks gas limits, verifies signatures
3. Bundles multiple UserOps into a single EntryPoint.handleOps() transaction
4. Pays the gas from its own EOA, getting reimbursed from the UserOp's gas payment (prefund deposit or paymaster)
5. Handles nonce management, retry logic, and mempool ordering
