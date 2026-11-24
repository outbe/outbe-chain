// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @notice Stand-in for the IntexFactory precompile that records the local proceeds hand-over.
/// @dev Tests `vm.etch` its code at `INTEX_FACTORY`, so it keeps no constructor state.
contract MockIntexFactory {
    uint256 public calls;
    address public lastCaller;
    uint32 public lastEmissionDay;
    uint32 public lastPageIndex;
    uint256 public lastAmount;

    function receiveLocalProceeds(uint32 emissionDay, uint32 pageIndex, uint256 amount) external {
        calls++;
        (lastCaller, lastEmissionDay, lastPageIndex, lastAmount) = (msg.sender, emissionDay, pageIndex, amount);
    }
}
