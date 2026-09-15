// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {BidsRelayProgress} from "../TargetRouterStorage.sol";

/// @title BidsRelay
/// @author Outbe
/// @notice Shared reading of a bids relay's progress, so the inbound library and the permissionless push
///         decide alike.
library BidsRelay {
    /// @notice Whether a finished round owes the origin a remainder report. A round that sent nothing has
    ///         nothing to recover from: the origin would answer with the same budget for the same outcome,
    ///         and that is a loop. A finished day is reported by its completeness marker instead.
    function advanced(BidsRelayProgress storage self, uint16 batchBefore) internal view returns (bool) {
        return !self.done && self.nextBatch > batchBefore;
    }
}
