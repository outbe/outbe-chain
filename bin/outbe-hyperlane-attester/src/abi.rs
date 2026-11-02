//! Contract ABI surface used by the Hyperlane attester.

use alloy_sol_types::sol;

sol!("../../contracts/precompiles/src/ICrosschainGov.sol");

sol! {
    /// Hyperlane ValidatorAnnounce: where each validator publishes its checkpoint bucket.
    interface IValidatorAnnounce {
        function getAnnouncedStorageLocations(address[] calldata validators)
            external
            view
            returns (string[][] memory);
    }
}
