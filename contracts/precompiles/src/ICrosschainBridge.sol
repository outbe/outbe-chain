// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @dev The CrosschainBridge precompile. Matches `CROSSCHAIN_BRIDGE_ADDRESS` in
///      crates/blockchain/primitives/src/addresses.rs.
address constant CROSSCHAIN_BRIDGE = 0x000000000000000000000000000000000000EE15;

/// CrosschainBridge precompile at `CROSSCHAIN_BRIDGE` (OIP-00369).
///
/// The owner of every multi-chain contract: on Outbe directly, on another chain
/// through its Interchain Account. It has no vote of its own. An upgrade or a configuration
/// change of our contracts is the `upgradeContracts` vote of IUpdate, and a Hyperlane chain
/// change is a vote of IHyperlaneController. Both call `callLocal` and `callRemote` here.
interface ICrosschainBridge {
    /// One owner call.
    struct Call {
        address to;
        uint256 value;
        bytes data;
    }

    /// Caller-facing domain revert.
    error CallerNotAllowed(address caller);
    error NotInitialized();
    error InvalidAddress();
    error InvalidDomain();
    error LocalDomainNotAllowed(uint32 domain);
    error EmptyCalls();
    error ZeroFund();
    error InsufficientBalance(uint256 required, uint256 available);
    error UndecodableReturn(string value);

    event RemoteCallDispatched(uint32 indexed domain, bytes32 indexed messageId, uint256 fee);
    event LocalCallExecuted(address indexed target, uint256 value);
    event Funded(address indexed from, uint256 amount);

    /// Tops up the balance that pays Interchain Account dispatch fees (IGP quote).
    function fund() external payable;

    /// Calls `call.to` on Outbe as the owner. The owner balance pays `call.value`. Only a
    /// precompile in the allowed-caller list may call it: the HyperlaneController and Update.
    function callLocal(Call calldata call) external returns (bytes memory);

    /// Runs `calls` on `domain` through the Interchain Account of the owner, in one message.
    /// The owner balance pays the dispatch fee. Same caller list as `callLocal`.
    function callRemote(uint32 domain, Call[] calldata calls) external returns (bytes32 messageId);

    /// Interchain Account of the owner on `domain`. It executes every remote call of the owner,
    /// so it is the governance owner of contracts on that chain.
    function remoteAccount(uint32 domain) external view returns (address);
}
