// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockERC20} from "@test-mocks/MockERC20.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

contract EscrowAdapterDecimalsTest is Test {
    EscrowAdapter internal escrow;
    MockTheCompact internal compact;

    address internal admin = address(1);
    address internal bridger = address(2);
    address internal auction = address(3);

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, bridger);
        compact = new MockTheCompact();
    }

    function _scale() internal view returns (uint256) {
        return escrow.getAssetVersion(escrow.currentAssetVersion()).paymentScale;
    }

    function test_wire_scalesAnEighteenDecimalToken() public {
        MockWCOEN token = new MockWCOEN();
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(token));
        assertEq(address(escrow.paymentToken()), address(token));
        assertEq(_scale(), 1e12);
    }

    function test_wire_scalesASixDecimalToken() public {
        MockERC20 token = new MockERC20("Token", "TKN", 6);
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(token));
        assertEq(_scale(), 1);
    }

    function test_wire_rejectsFewerDecimalsThanTheProtocol() public {
        MockERC20 token = new MockERC20("Token", "TKN", 5);
        vm.prank(admin);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.PaymentTokenDecimals.selector, uint8(5)));
        escrow.wire(auction, address(compact), address(token));
    }

    function test_rotation_keepsTheRetiredScale() public {
        MockWCOEN token = new MockWCOEN();
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(token));

        MockERC20 rotated = new MockERC20("Token", "TKN", 6);
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(rotated));

        assertEq(escrow.getAssetVersion(0).paymentScale, 1e12, "retired scale");
        assertEq(_scale(), 1, "active scale");
    }
}
