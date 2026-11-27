// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";

contract RoutersUupsTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 1;

    address internal stranger = makeAddr("stranger");
    address internal erc20Router = makeAddr("erc20Router");
    address internal auction = makeAddr("auction");
    address internal intex = makeAddr("intex");
    address internal escrow = makeAddr("escrow");
    address internal ica = makeAddr("outbeIca");

    IntexAuctionTargetRouter internal target;

    function setUp() public {
        _setUpBridge();

        target = DeployProxy.targetRouter(
            address(bridge),
            address(this),
            DeployProxy.TargetRouterLinks(OUTBE_CHAIN_ID, erc20Router, auction, intex, escrow, ica)
        );
    }

    function _impl() internal returns (IntexAuctionTargetRouter) {
        return new IntexAuctionTargetRouter(address(bridge), OUTBE_CHAIN_ID, erc20Router, auction, intex, escrow, ica);
    }

    function test_Initialize_SetsOwner() public view {
        assertEq(target.owner(), address(this));
    }

    function test_RevertWhen_InitializeCalledTwice() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        target.initialize(stranger);
    }

    function test_RevertWhen_ImplementationInitialized() public {
        IntexAuctionTargetRouter timpl = _impl();
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        timpl.initialize(address(this));
    }

    function test_RevertWhen_InitializeZeroOwner() public {
        IntexAuctionTargetRouter impl = _impl();
        bytes memory initData = abi.encodeCall(IntexAuctionTargetRouter.initialize, (address(0)));
        vm.expectRevert(abi.encodeWithSignature("ZeroAddress(string)", "owner"));
        new ERC1967Proxy(address(impl), initData);
    }

    function test_RevertWhen_UpgradeByNonOwner() public {
        IntexAuctionTargetRouter newImpl = _impl();
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, stranger));
        target.upgradeToAndCall(address(newImpl), "");
    }

    function test_Upgrade_PreservesOwnerAndImmutables() public {
        IntexAuctionTargetRouter newImpl = _impl();
        target.upgradeToAndCall(address(newImpl), "");

        bytes32 implSlot = vm.load(address(target), ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), address(newImpl));
        assertEq(address(target.intexAuction()), auction);
        assertEq(address(target.intexNFT1155()), intex);
        assertEq(address(target.escrowAdapter()), escrow);
        assertEq(target.outbeIca(), ica);
        assertEq(target.owner(), address(this));
        assertEq(address(target.BRIDGE()), address(bridge));
        assertEq(address(target.erc20Router()), erc20Router);
        assertEq(target.OUTBE_CHAIN_ID(), OUTBE_CHAIN_ID);
    }
}
