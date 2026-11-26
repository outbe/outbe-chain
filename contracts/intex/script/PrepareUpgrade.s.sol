// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {UpgradeProposal} from "@shared/UpgradeProposal.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IntexCreate3} from "./IntexCreate3.sol";
import {IntexScript} from "./IntexScript.sol";

/// @dev Prepares an upgrade of the five Intex proxies on every chain of the stack (see
///      UpgradeProposal). Each implementation gets every immutable of the live proxy, read through
///      its readers, so an upgrade cannot change a link.
///
/// Env: the UpgradeProposal ones, `CREATE3_FACTORY_ADDRESS` and `CONTRACT_SALT`.
contract PrepareUpgrade is IntexScript, UpgradeProposal {
    function _proxies(address deployer) internal view override returns (address[] memory proxies) {
        address[5] memory stackProxies = IntexCreate3.proxies(stack(deployer));
        proxies = new address[](stackProxies.length);
        for (uint256 i = 0; i < stackProxies.length; i++) {
            proxies[i] = stackProxies[i];
        }
    }

    function _newImplementation(uint256 index, address proxy) internal override returns (address) {
        if (index == 0) {
            IntexNFT1155 nft = IntexNFT1155(proxy);
            return address(
                new IntexNFT1155(
                    nft.targetRouter(),
                    nft.intexNFT1155Router(),
                    nft.intexAuction(),
                    nft.intexFactory(),
                    nft.gemFactory()
                )
            );
        }
        if (index == 1) {
            IntexNFT1155Router nftRouter = IntexNFT1155Router(payable(proxy));
            return address(new IntexNFT1155Router(address(nftRouter.token()), address(nftRouter.BRIDGE())));
        }
        if (index == 2) {
            EscrowAdapter escrow = EscrowAdapter(payable(proxy));
            return address(new EscrowAdapter(escrow.intexAuction(), escrow.targetRouter()));
        }
        if (index == 3) {
            IntexAuction auction = IntexAuction(proxy);
            return address(
                new IntexAuction(auction.targetRouter(), address(auction.escrow()), address(auction.intexContract()))
            );
        }
        IntexAuctionTargetRouter router = IntexAuctionTargetRouter(payable(proxy));
        return address(
            new IntexAuctionTargetRouter(
                address(router.BRIDGE()),
                router.OUTBE_CHAIN_ID(),
                address(router.erc20Router()),
                address(router.intexAuction()),
                address(router.intexContract()),
                address(router.escrow()),
                router.outbeIca()
            )
        );
    }

    function _deployerOwns(address proxy, address deployer) internal view override returns (bool) {
        return Ownable(proxy).owner() == deployer;
    }
}
