// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

/// @dev Proceeds-recipient configuration and its finalize guard.
contract EscrowAdapterProceedsTest is Test {
    EscrowAdapter escrow;
    MockTheCompact compact;
    MockWCOEN paymentToken;

    address admin = address(1);
    address bridger = address(2);
    address auction = address(3);
    address bidder1 = address(5);
    address outsider = address(7);

    uint32 worldwideDay1 = 1;
    bytes32 constant GUID = bytes32(uint256(0xDEADBEEF));

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, bridger);
        compact = new MockTheCompact();
        paymentToken = new MockWCOEN();

        vm.prank(admin);
        escrow.wire(auction, address(compact), address(paymentToken));
        compact.setResetPeriodSeconds(0);

        paymentToken.mint(bidder1, 10_000 * 10 ** 6);
        vm.prank(bidder1);
        paymentToken.approve(address(escrow), type(uint256).max);
    }

    function test_RevertWhen_FinalizeWithProceedsAndNoRecipient() public {
        // One Intex at the full rate of a 1-unit basis locks 1e18, all of it paid at a full-rate clearing.
        paymentToken.mint(bidder1, 1e18);
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, 1e18, 1_000_000, 1);

        address[] memory winners = new address[](1);
        winners[0] = bidder1;

        vm.expectRevert(IEscrowAdapter.ProceedsRecipientNotSet.selector);
        vm.prank(bridger);
        escrow.finalizeAuction(worldwideDay1, GUID, winners, 0, 0, 1_000_000, 1_000_000, true);
    }

    function test_SetProceedsRecipient_OnlyAdmin() public {
        vm.expectRevert();
        vm.prank(outsider);
        escrow.setProceedsRecipient(outsider);

        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "recipient"));
        vm.prank(admin);
        escrow.setProceedsRecipient(address(0));
    }
}
