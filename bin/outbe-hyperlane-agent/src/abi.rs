//! Contract ABI surface used by the Hyperlane agent.

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

    /// Hyperlane Mailbox: dispatches messages on the origin chain and
    /// processes them on the destination chain.
    interface IMailbox {
        event Dispatch(address indexed sender, uint32 indexed destination, bytes32 indexed recipient, bytes message);
        function delivered(bytes32 messageId) external view returns (bool);
        function recipientIsm(address recipient) external view returns (address);
        function process(bytes calldata metadata, bytes calldata message) external payable;
    }

    /// The Hyperlane ISM views that the relayer reads on the destination chain.
    interface IIsm {
        function moduleType() external view returns (uint8);
        /// Routing ISM: the ISM for this message.
        function route(bytes calldata message) external view returns (address);
        /// Multisig ISM: the validators and the number of signatures.
        function validatorsAndThreshold(bytes calldata message)
            external
            view
            returns (address[] memory validators, uint8 threshold);
    }
}
