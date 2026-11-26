// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
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
        compact = new MockTheCompact();
    }

    function _deploy(address token) internal {
        escrow = DeployProxy.escrowAdapter(admin, auction, bridger, address(compact), token);
    }

    function _scale() internal view returns (uint256) {
        return escrow.getIntexAuctionAssetVersion(escrow.currentIntexAuctionAssetVersion()).assetScale;
    }

    function test_initialize_scalesAnEighteenDecimalToken() public {
        MockWCOEN token = new MockWCOEN();
        _deploy(address(token));
        assertEq(address(escrow.intexAuctionAsset()), address(token));
        assertEq(_scale(), 1e12);
    }

    function test_initialize_scalesASixDecimalToken() public {
        _deploy(address(new MockERC20("Token", "TKN", 6)));
        assertEq(_scale(), 1);
    }

    function test_initialize_rejectsFewerDecimalsThanTheProtocol() public {
        MockERC20 token = new MockERC20("Token", "TKN", 5);
        EscrowAdapter impl = new EscrowAdapter(auction, bridger);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.IntexAuctionAssetDecimals.selector, uint8(5)));
        new ERC1967Proxy(
            address(impl), abi.encodeCall(EscrowAdapter.initialize, (admin, address(compact), address(token)))
        );
    }

    function test_rotation_rejectsFewerDecimalsThanTheProtocol() public {
        _deploy(address(new MockWCOEN()));
        MockERC20 token = new MockERC20("Token", "TKN", 5);
        vm.prank(admin);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.IntexAuctionAssetDecimals.selector, uint8(5)));
        escrow.setIntexAuctionAsset(address(compact), address(token));
    }

    function test_rotation_keepsTheRetiredScale() public {
        _deploy(address(new MockWCOEN()));

        MockERC20 rotated = new MockERC20("Token", "TKN", 6);
        vm.prank(admin);
        escrow.setIntexAuctionAsset(address(compact), address(rotated));

        assertEq(escrow.getIntexAuctionAssetVersion(0).assetScale, 1e12, "retired scale");
        assertEq(_scale(), 1, "active scale");
    }
}
