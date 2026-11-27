// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {INTEX_FACTORY} from "@precompiles/IIntexFactory.sol";
import {GEM_FACTORY} from "@precompiles/IGemFactory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {DeployIntexNFT1155} from "./1_DeployIntexNFT1155.s.sol";
import {DeployIntexNFT1155Router} from "./2_DeployIntexNFT1155Router.s.sol";
import {DeployEscrowAdapter} from "./3_DeployEscrowAdapter.s.sol";
import {DeployIntexAuction} from "./4_DeployIntexAuction.s.sol";
import {DeployIntexAuctionTargetRouter} from "./5_DeployIntexAuctionTargetRouter.s.sol";
import {IntexCreate3} from "./IntexCreate3.sol";

/// @dev Deploys the five Intex contracts on one chain, Outbe included, each a UUPS proxy at its
///      CREATE3 address owned by the deployer, then reads every link back. Every link is an
///      implementation immutable taken from the predicted addresses, so nothing is wired after
///      the deploy. A proxy that already exists is kept, and a re-run only checks.
///
///   1. IntexNFT1155
///   2. IntexNFT1155Router, paired with the other chains of `REMOTE_CHAIN_IDS`
///   3. EscrowAdapter, with WCOEN in The Compact
///   4. IntexAuction
///   5. IntexAuctionTargetRouter, for the Interchain Account of Outbe on this chain
///
/// Hyperlane must know this chain before this runs. 6_HandOver gives the contracts to governance.
///
/// Env: `DEPLOYER_PK`, `CREATE3_FACTORY_ADDRESS`, `CONTRACT_SALT`, `OUTBE_CHAIN_ID`, `OUTBE_RPC_URL`,
///   `BRIDGE_ADDRESS`, `WCOEN_BRIDGE`, `COMPACT_ADDRESS`, `WCOEN_TOKEN`, `REMOTE_CHAIN_IDS`.
contract DeployAll is
    DeployIntexNFT1155,
    DeployIntexNFT1155Router,
    DeployEscrowAdapter,
    DeployIntexAuction,
    DeployIntexAuctionTargetRouter
{
    error LinkMismatch(address proxy, string link);

    struct Config {
        bool outbe;
        address compact;
        address wcoen;
        uint256[] remoteChainIds;
        Transport transport;
    }

    function run()
        public
        override(
            DeployIntexNFT1155,
            DeployIntexNFT1155Router,
            DeployEscrowAdapter,
            DeployIntexAuction,
            DeployIntexAuctionTargetRouter
        )
    {
        uint256 pk = vm.envUint("DEPLOYER_PK");
        address deployer = vm.addr(pk);
        IntexCreate3.Stack memory s = stack(deployer);
        // Read before the broadcast: off Outbe the Interchain Account comes from the Outbe node.
        Config memory c = config();
        console.log("Salt:", saltVersion());

        vm.startBroadcast(pk);
        deployAll(deployer, s, c);
        vm.stopBroadcast();

        checkLinks(s, c);
        console.log("IntexNFT1155:", s.intexNFT1155);
        console.log("IntexNFT1155Router:", s.intexNFT1155Router);
        console.log("EscrowAdapter:", s.escrowAdapter);
        console.log("IntexAuction:", s.intexAuction);
        console.log("IntexAuctionTargetRouter:", s.intexAuctionTargetRouter);
        console.log("=== DeployAll complete ===");
    }

    function config() public returns (Config memory) {
        return Config({
            outbe: onOutbe(),
            compact: vm.envAddress("COMPACT_ADDRESS"),
            wcoen: vm.envAddress("WCOEN_TOKEN"),
            remoteChainIds: vm.envUint("REMOTE_CHAIN_IDS", ","),
            transport: transport()
        });
    }

    /// @dev Requires an active broadcast from `deployer`.
    function deployAll(address deployer, IntexCreate3.Stack memory s, Config memory c) public {
        deployIntexNFT1155(deployer, s, c.outbe);
        deployIntexNFT1155Router(deployer, s, c.transport.bridge, c.remoteChainIds);
        deployEscrowAdapter(deployer, s, c.compact, c.wcoen);
        deployIntexAuction(deployer, s);
        deployIntexAuctionTargetRouter(deployer, s, c.transport);
    }

    /// @dev Reverts unless every proxy of `s` holds the links `c` asks for.
    function checkLinks(IntexCreate3.Stack memory s, Config memory c) public view {
        IntexNFT1155 nft = IntexNFT1155(s.intexNFT1155);
        _check(s.intexNFT1155, nft.intexAuctionTargetRouter() == s.intexAuctionTargetRouter, "intexAuctionTargetRouter");
        _check(s.intexNFT1155, nft.intexNFT1155Router() == s.intexNFT1155Router, "intexNFT1155Router");
        _check(s.intexNFT1155, nft.intexAuction() == s.intexAuction, "intexAuction");
        _check(s.intexNFT1155, nft.intexFactory() == (c.outbe ? INTEX_FACTORY : address(0)), "intexFactory");
        _check(s.intexNFT1155, nft.gemFactory() == (c.outbe ? GEM_FACTORY : address(0)), "gemFactory");

        IntexNFT1155Router nftRouter = IntexNFT1155Router(payable(s.intexNFT1155Router));
        _check(s.intexNFT1155Router, address(nftRouter.token()) == s.intexNFT1155, "token");
        _check(s.intexNFT1155Router, address(nftRouter.BRIDGE()) == c.transport.bridge, "BRIDGE");
        for (uint256 i = 0; i < c.remoteChainIds.length; i++) {
            uint256 chainId = c.remoteChainIds[i];
            if (chainId == block.chainid) continue;
            bytes memory expected = InteroperableAddress.formatEvmV1(chainId, s.intexNFT1155Router);
            // forge-lint: disable-next-line(unsafe-typecast) -- remoteRouters checked the chain id fits uint32
            _check(
                s.intexNFT1155Router,
                keccak256(nftRouter.remoteRouter(uint32(chainId))) == keccak256(expected),
                "remoteRouter"
            );
        }

        EscrowAdapter escrow = EscrowAdapter(payable(s.escrowAdapter));
        _check(s.escrowAdapter, escrow.intexAuction() == s.intexAuction, "intexAuction");
        _check(
            s.escrowAdapter, escrow.intexAuctionTargetRouter() == s.intexAuctionTargetRouter, "intexAuctionTargetRouter"
        );
        _check(s.escrowAdapter, address(escrow.compact()) == c.compact, "compact");
        _check(s.escrowAdapter, address(escrow.intexAuctionAsset()) == c.wcoen, "intexAuctionAsset");

        IntexAuction auction = IntexAuction(s.intexAuction);
        _check(
            s.intexAuction, auction.intexAuctionTargetRouter() == s.intexAuctionTargetRouter, "intexAuctionTargetRouter"
        );
        _check(s.intexAuction, address(auction.escrowAdapter()) == s.escrowAdapter, "escrowAdapter");
        _check(s.intexAuction, address(auction.intexNFT1155()) == s.intexNFT1155, "intexNFT1155");

        IntexAuctionTargetRouter router = IntexAuctionTargetRouter(payable(s.intexAuctionTargetRouter));
        _check(s.intexAuctionTargetRouter, address(router.intexAuction()) == s.intexAuction, "intexAuction");
        _check(s.intexAuctionTargetRouter, address(router.intexNFT1155()) == s.intexNFT1155, "intexNFT1155");
        _check(s.intexAuctionTargetRouter, address(router.escrowAdapter()) == s.escrowAdapter, "escrowAdapter");
        _check(s.intexAuctionTargetRouter, router.outbeIca() == c.transport.outbeIca, "outbeIca");
        _check(s.intexAuctionTargetRouter, address(router.erc20Router()) == c.transport.wcoenBridge, "erc20Router");
        _check(s.intexAuctionTargetRouter, address(router.BRIDGE()) == c.transport.bridge, "BRIDGE");
        _check(s.intexAuctionTargetRouter, router.OUTBE_CHAIN_ID() == c.transport.outbeChainId, "OUTBE_CHAIN_ID");
    }

    function _check(address proxy, bool holds, string memory link) private pure {
        if (!holds) revert LinkMismatch(proxy, link);
    }
}
