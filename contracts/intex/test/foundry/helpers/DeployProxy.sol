// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";
import {TargetMessenger} from "@contracts/target/TargetMessenger.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {Vm} from "forge-std/Vm.sol";

/// @dev Deploys implementation + ERC1967 proxy pairs for the UUPS contracts under test.
///      Returned handles point at the proxy; `bridger` receives RELAYER_ROLE.
library DeployProxy {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    function intexNFT1155(address defaultAdmin, address bridger) internal returns (IntexNFT1155) {
        IntexNFT1155 impl = new IntexNFT1155();
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(IntexNFT1155.initialize, (defaultAdmin)));
        IntexNFT1155 nft = IntexNFT1155(address(proxy));
        bytes32 role = nft.RELAYER_ROLE();
        vm.prank(defaultAdmin);
        nft.grantRole(role, bridger);
        return nft;
    }

    function intexAuction(address defaultAdmin, address bridger) internal returns (IntexAuction) {
        IntexAuction impl = new IntexAuction();
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(IntexAuction.initialize, (defaultAdmin)));
        IntexAuction auction = IntexAuction(address(proxy));
        bytes32 role = auction.RELAYER_ROLE();
        vm.prank(defaultAdmin);
        auction.grantRole(role, bridger);
        return auction;
    }

    function escrowAdapter(address defaultAdmin, address bridger) internal returns (EscrowAdapter) {
        EscrowAdapter impl = new EscrowAdapter();
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(EscrowAdapter.initialize, (defaultAdmin)));
        EscrowAdapter escrow = EscrowAdapter(address(proxy));
        bytes32 role = escrow.RELAYER_ROLE();
        vm.prank(defaultAdmin);
        escrow.grantRole(role, bridger);
        return escrow;
    }

    function originMessenger(address bridge, address delegate, uint32 bnbChainId) internal returns (OriginMessenger) {
        OriginMessenger impl = new OriginMessenger(bridge, bnbChainId);
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(OriginMessenger.initialize, (delegate)));
        return OriginMessenger(payable(address(proxy)));
    }

    function targetMessenger(address bridge, address delegate, uint32 outbeChainId) internal returns (TargetMessenger) {
        TargetMessenger impl = new TargetMessenger(bridge, outbeChainId);
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(TargetMessenger.initialize, (delegate)));
        return TargetMessenger(payable(address(proxy)));
    }

    function onftAdapter(address tokenAddr, address lzEndpoint, address delegate) internal returns (ONFT1155Adapter) {
        ONFT1155Adapter impl = new ONFT1155Adapter(tokenAddr, lzEndpoint);
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(ONFT1155Adapter.initialize, (delegate)));
        return ONFT1155Adapter(payable(address(proxy)));
    }

    function onftAdapterBatch(address tokenAddr, address bridge, address delegate)
        internal
        returns (ONFT1155AdapterBatch)
    {
        ONFT1155AdapterBatch impl = new ONFT1155AdapterBatch(tokenAddr, bridge);
        ERC1967Proxy proxy =
            new ERC1967Proxy(address(impl), abi.encodeCall(ONFT1155AdapterBatch.initialize, (delegate)));
        return ONFT1155AdapterBatch(payable(address(proxy)));
    }
}
