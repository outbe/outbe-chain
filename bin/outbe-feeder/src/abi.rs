//! Precompile ABI surface used by the feeder.

use alloy_sol_types::sol;

sol!("../../contracts/precompiles/src/IOracle.sol");

sol!("../../contracts/precompiles/src/IValidatorSet.sol");

sol!("../../contracts/precompiles/src/IHyperlaneController.sol");

sol! {
    /// Hyperlane ValidatorAnnounce: where each validator publishes its checkpoint bucket.
    interface IValidatorAnnounce {
        function getAnnouncedStorageLocations(address[] calldata validators)
            external
            view
            returns (string[][] memory);
    }
}
