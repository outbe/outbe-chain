//! Canonical TeeRegistry V1 ABI shared by the native precompile and host
//! operator tooling. Keeping one generated interface pins selectors and tuple
//! layout across consensus and transaction construction.

use alloy_sol_types::sol;

sol! {
    #[derive(Debug, PartialEq, Eq)]
    struct NodeEnclaveBindingV1View {
        bool exists;
        bytes32 nodeIdHash;
        bytes32 enclaveId;
        bytes32 bindingId;
        bytes32 intentHash;
        bytes32 evidenceHash;
        bytes32 policyHash;
        uint64 bindingVersion;
        uint64 registrationVersion;
        uint64 renewalNonce;
        uint64 transitionNonce;
        uint64 leaseStartedAt;
        uint64 validUntil;
        uint64 collateralValidUntil;
        bytes32 recipientX25519;
        bytes32 attestationEd25519;
        bytes32 noiseResponderX25519;
        bytes32 mrenclave;
        bytes32 mrsigner;
        uint16 isvProdId;
        uint16 isvSvn;
        uint8 platformTcbStatus;
        bytes32 verdictHash;
        bytes32 nodeHostAuthorizationHash;
    }

    interface ITeeRegistryV1 {
        event OfferKeySealedForRegistryV1(
            bytes32 indexed nodeIdHash,
            bytes sealedOfferKey
        );

        function isBootstrapped() external view returns (bool);
        function tributeOfferPublicKey() external view returns (uint256);
        function policyHash() external view returns (uint256);
        function keyEpoch() external view returns (uint256);
        function tributeOfferEpoch() external view returns (uint256);
        function activePolicyV1() external view returns (bytes memory);
        function stagedSuccessorPolicyV1()
            external
            view
            returns (bool exists, uint256 proposalId, bytes memory policy);

        function registerEnclave(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature,
            bytes calldata validatorNodeBinding,
            bytes calldata validatorSignature,
            bytes calldata nodeBindingSignature
        ) external returns (bool);

        function renewEnclave(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature
        ) external returns (bool);

        function replaceEnclaveBinding(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature
        ) external returns (bool);

        function transitionEnclaveMeasurement(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature
        ) external returns (bool);

        function validatorEnclaveBinding(address validator)
            external
            view
            returns (NodeEnclaveBindingV1View memory);

        function nodeHostEnclaveBinding(uint8 rethP2pPrefix, bytes32 rethP2pX)
            external
            view
            returns (NodeEnclaveBindingV1View memory);

        function isValidatorEnclaveReady(address validator) external view returns (bool);

        function isNodeHostEnclaveReady(uint8 rethP2pPrefix, bytes32 rethP2pX)
            external
            view
            returns (bool);
    }
}
