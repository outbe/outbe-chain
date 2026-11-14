use alloy_primitives::{address, Address};

// ---------------------------------------------------------------------------
// Protocol core chain addresses
// ---------------------------------------------------------------------------

/// Validator set management precompile address.
pub const VALIDATOR_SET_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE00");

/// Rewards precompile address.
pub const REWARDS_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE03");

/// Log emitter address for zero-fee policy rejections.
///
/// When the executor rejects a zero-fee user transaction at stateless
/// classification or stateful authorization time, it includes the transaction
/// in the block with a `status=0` receipt carrying one `OutbeFailure(code, reason)`
/// log emitted from this address. No EVM execution and no state mutation happen
/// at this address; it is a logical namespace for `eth_getLogs` filtering.
pub const ZERO_FEE_POLICY_LOG_ADDRESS: Address =
    address!("0x000000000000000000000000000000000000EE06");

/// ZeroFee paymaster precompile address (stateful).
///
/// Acts as the EIP-7702 delegation target for void sponsorship. Users who
/// delegate their EOA to this address may submit sponsored transactions within
/// configured daily and envelope limits. This address holds no native balance;
/// the executor's pre-fee hook skips fee debit when EIP-7702 delegation to this
/// address is authorized.
pub const ZEROFEE_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE09");

/// TEE Registry precompile address (storage-backed KV).
///
/// Records per-validator TEE registration bundles and the global threshold
/// `network_encryption_public_key`. The `TeeBootstrap` system transaction writes
/// the network encryption key once through `StorageHandle::contract`, while the
/// public ABI exposes view methods and caller-bound enclave lifecycle methods.
pub const TEE_REGISTRY_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE0A");

/// On-chain upgrade governance precompile address.
///
/// Hosts proposal/vote state and the active protocol version.
pub const UPDATE_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE0B");

/// CodeRegistry precompile address: permissionless append-only registry of
/// public Heartwood repository ids and approved code-change receipts.
pub const CODE_REGISTRY_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE11");

/// Genesis-reserved two-byte class for dynamic precompile accounts.
pub const RESERVED_DYNAMIC_PRECOMPILE_PREFIX: [u8; 2] = [0x53, 0xc0];

/// Returns whether an address belongs to the reserved dynamic precompile class.
pub const fn is_reserved_dynamic_precompile_address(address: Address) -> bool {
    address.0 .0[0] == RESERVED_DYNAMIC_PRECOMPILE_PREFIX[0]
        && address.0 .0[1] == RESERVED_DYNAMIC_PRECOMPILE_PREFIX[1]
}

/// System address used for system-only calls (block hooks).
pub const SYSTEM_ADDRESS: Address = Address::ZERO;

/// Reserved sender/recipient address for signed system-transaction artifacts.
///
/// System transactions are first-class block-body transactions used to expose
/// begin-block runtime effects through normal Ethereum receipts. The proposer
/// signature authenticates them, but they execute in EVM system mode via
/// [`SYSTEM_ADDRESS`]. So this address must remain distinct from both the fee
/// recipient and any user-facing precompile account.
pub const OUTBE_SYSTEM_TX_ADDRESS: Address = address!("0xff00000000000000000000000000000000000001");

// ---------------------------------------------------------------------------
// Pre-deployed external standard contracts (no precompile dispatch)
// ---------------------------------------------------------------------------
//
// These addresses hold canonical third-party runtime bytecode pre-allocated at
// genesis. They execute as ordinary EVM accounts under standard semantics.

/// Arachnid deterministic CREATE2 deployer.
///
/// Accepts calldata `salt(32) || initcode` and forwards via CREATE2, producing
/// a contract at `keccak256(0xff || deployer || salt || keccak256(initcode))[12:]`.
/// Source: `Arachnid/deterministic-deployment-proxy`.
pub const CREATE2_DEPLOYER_ADDRESS: Address =
    address!("0x4e59b44847b379578588920cA78FbF26c0B4956C");

/// ERC-4337 EntryPoint v0.7.
///
/// Canonical bundler entry point for account-abstraction `UserOperation`s.
/// Source: `eth-infinitism/account-abstraction@v0.7.0`.
pub const ENTRY_POINT_V07_ADDRESS: Address = address!("0x0000000071727De22E5E9d8BAf0edAc6f37da032");

/// ERC-4337 SenderCreator v0.7.
///
/// Helper invoked by EntryPoint v0.7 to deploy AA accounts from factory
/// `initCode`. Address is the deterministic CREATE address from EntryPoint's
/// constructor: `keccak256(rlp([entrypoint, 1]))[12:]`. Pre-deployed at genesis
/// because Outbe skips the EntryPoint constructor (which would otherwise emit
/// `new SenderCreator()`).
pub const SENDER_CREATOR_V07_ADDRESS: Address =
    address!("0xEFC2c1444eBCC4Db75e7613d20C6a62fF67A167C");
