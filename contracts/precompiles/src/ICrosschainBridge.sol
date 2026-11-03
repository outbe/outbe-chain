// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @dev The CrosschainGov precompile. Matches `CROSSCHAIN_GOV_ADDRESS` in
///      crates/blockchain/primitives/src/addresses.rs.
address constant CROSSCHAIN_GOV = 0x000000000000000000000000000000000000EE15;

/// CrosschainGov precompile at `CROSSCHAIN_GOV` (OIP-00369).
///
/// The owner of every multi-chain contract: on Outbe directly, on another chain
/// through its Interchain Account. The crosschain vote target approves owner
/// actions, and `applyApproved` runs them (see `crates/system/crosschaingov` for
/// the JSON payloads).
interface ICrosschainGov {
    /// One owner call.
    struct Call {
        address to;
        uint256 value;
        bytes data;
    }

    /// Caller-facing domain revert.
    error NotOwnerCaller(address caller);
    error NotInitialized();
    error InvalidAddress();
    error InvalidDomain();
    error LocalDomainNotAllowed(uint32 domain);
    error EmptyCalls();
    error ZeroFund();
    error InsufficientBalance(uint256 required, uint256 available);
    error UndecodableReturn(string value);
    error UnknownApprovedAction(uint256 proposalId);
    error ApprovedPayloadMismatch(uint256 proposalId);

    event RemoteCallDispatched(uint32 indexed domain, bytes32 indexed messageId, uint256 fee);
    event LocalCallExecuted(address indexed target, uint256 value);
    event Funded(address indexed from, uint256 amount);
    event ApprovedActionApplied(uint256 indexed proposalId);
    event ApprovedActionExpired(uint256 indexed proposalId);

    /// Tops up the balance that pays Interchain Account dispatch fees (IGP quote).
    function fund() external payable;

    /// Runs a crosschain action that a governance vote approved. Permissionless: the vote decided,
    /// and `payload` must be the approved JSON (its keccak256 is stored). An action older than the
    /// governance voting window is dropped with `ApprovedActionExpired`, so a stale approval cannot
    /// undo a newer one. A remote action pays the Hyperlane fee from the owner balance.
    function applyApproved(uint256 proposalId, bytes calldata payload) external;

    /// Calls `call.to` on Outbe as the owner. The owner balance pays `call.value`. Only a
    /// precompile in the owner-caller list may call it: today the HyperlaneController and the
    /// owner itself.
    function callLocal(Call calldata call) external returns (bytes memory);

    /// Runs `calls` on `domain` through the Interchain Account of the owner, in one message.
    /// The owner balance pays the dispatch fee. Same caller list as `callLocal`.
    function callRemote(uint32 domain, Call[] calldata calls) external returns (bytes32 messageId);

    /// Interchain Account of the owner on `domain`. It executes every remote call of the owner,
    /// so it is the governance owner of contracts on that chain.
    function remoteAccount(uint32 domain) external view returns (address);
}
