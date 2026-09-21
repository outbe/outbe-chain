// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// HyperlaneController precompile at 0x000000000000000000000000000000000000EE14.
///
/// Governance-owned controller of the Hyperlane bridge. Holds only the local
/// InterchainAccountRouter address and a `domain -> StorageMessageIdMultisigIsm`
/// table (the Outbe chain included under its own domain); validator sets live
/// in the ISMs themselves. Every mutation except `initialize` and `fund` is
/// applied by the validator vote target (see the crate README for the JSON payloads).
interface IHyperlaneController {
    event Initialized(address indexed router);
    event DomainAdded(uint32 indexed domain, address indexed ism);
    event DomainRemoved(uint32 indexed domain);
    event ValidatorsAndThresholdApplied(uint8 threshold, uint256 validatorCount);
    event RemoteCallDispatched(uint32 indexed domain, bytes32 indexed messageId, uint256 fee);
    event LocalCallExecuted(address indexed target, uint256 value);
    event Funded(address indexed from, uint256 amount);

    /// One-shot bootstrap. `domains[i] -> isms[i]` must include the Outbe
    /// domain (= chain id). Caller must be the current owner of the Outbe ISM
    /// (the deployer that staged `transferOwnership` to this precompile); the
    /// precompile accepts that pending ownership and verifies it owns `router`.
    function initialize(address router, uint32[] calldata domains, address[] calldata isms) external;

    /// Tops up the balance that pays Interchain Account dispatch fees (IGP quote).
    function fund() external payable;

    function router() external view returns (address);

    /// ISM for `domain`, or zero when not configured.
    function ismByDomain(uint32 domain) external view returns (address);

    function domains() external view returns (uint32[] memory);
}
