// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";

/// @title ConfigureTarget
/// @author Outbe
/// @notice Wires the auction stack of one target chain: the auction to its escrow, the escrow to
///         the auction, The Compact and wCOEN, and the router to all three, plus the relayer
///         roles of the router and the NFT bridge. Run it after DeployTarget, before HandOver,
///         on every target, Outbe included. A step that already holds is skipped, so a re-run
///         sends nothing.
/// @dev Env: DEPLOYER_PK, CREATE3_FACTORY_ADDRESS, CONTRACT_SALT (optional), COMPACT_ADDRESS,
///      WCOEN_TOKEN.
contract ConfigureTarget is BaseScript {
    struct Target {
        address nft;
        address escrow;
        address auction;
        address nftBridge;
        address router;
        address compact;
        address wcoen;
    }

    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PK");
        address deployer = vm.addr(pk);
        Create3Factory factory = create3Factory();
        Target memory t = Target({
            nft: predictProxy(factory, deployer, "IntexNFT1155"),
            escrow: predictProxy(factory, deployer, "EscrowAdapter"),
            auction: predictProxy(factory, deployer, "IntexAuction"),
            nftBridge: predictProxy(factory, deployer, "IntexNFT1155Bridge"),
            router: predictProxy(factory, deployer, "TargetRouter"),
            compact: vm.envAddress("COMPACT_ADDRESS"),
            wcoen: vm.envAddress("WCOEN_TOKEN")
        });

        vm.startBroadcast(pk);
        configure(deployer, t);
        vm.stopBroadcast();
    }

    /// @dev Requires an active broadcast from `deployer`.
    function configure(address deployer, Target memory t) public {
        requireDeployed(t.nft);
        requireDeployed(t.escrow);
        requireDeployed(t.auction);
        requireDeployed(t.nftBridge);
        requireDeployed(t.router);

        IntexAuction auction = IntexAuction(t.auction);
        if (address(auction.escrowContract()) != t.escrow) {
            requireDeployerAdmin(t.auction, deployer);
            auction.wire(t.escrow);
            console.log("IntexAuction wired");
        }

        EscrowAdapter escrow = EscrowAdapter(payable(t.escrow));
        if (
            escrow.intexAuctionContract() != t.auction || address(escrow.compact()) != t.compact
                || address(escrow.paymentToken()) != t.wcoen
        ) {
            requireDeployerAdmin(t.escrow, deployer);
            escrow.wire(t.auction, t.compact, t.wcoen);
            console.log("EscrowAdapter wired");
        }

        TargetRouter router = TargetRouter(payable(t.router));
        if (
            address(router.auction()) != t.auction || address(router.intex()) != t.nft
                || address(router.escrowAdapter()) != t.escrow
        ) {
            requireDeployerAdmin(t.router, deployer);
            router.wire(t.auction, t.nft, t.escrow);
            console.log("TargetRouter wired");
        }

        grantRoleIfMissing(t.auction, auction.RELAYER_ROLE(), t.router, deployer);
        grantRoleIfMissing(t.escrow, escrow.RELAYER_ROLE(), t.router, deployer);
        bytes32 relayer = IntexNFT1155(t.nft).RELAYER_ROLE();
        grantRoleIfMissing(t.nft, relayer, t.router, deployer);
        grantRoleIfMissing(t.nft, relayer, t.nftBridge, deployer);
    }
}
