//! Contract ABI surface used by the Hyperlane validator.

use alloy_sol_types::sol;

sol!("../../contracts/precompiles/src/IHyperlaneController.sol");
sol!("../../contracts/precompiles/src/IValidatorSet.sol");

sol! {
    /// Hyperlane ValidatorAnnounce: where each validator publishes its checkpoint bucket.
    interface IValidatorAnnounce {
        function getAnnouncedStorageLocations(address[] calldata validators)
            external
            view
            returns (string[][] memory);
        function mailbox() external view returns (address);
        function localDomain() external view returns (uint32);
        function announce(address validator, string calldata storageLocation, bytes calldata signature)
            external
            returns (bool);
    }

    /// Hyperlane MerkleTreeHook: the tree of dispatched message ids.
    interface IMerkleTreeHook {
        event InsertedIntoTree(bytes32 messageId, uint32 index);
        function latestCheckpoint() external view returns (bytes32 root, uint32 index);
        function mailbox() external view returns (address);
        function deployedBlock() external view returns (uint256);
    }
}
