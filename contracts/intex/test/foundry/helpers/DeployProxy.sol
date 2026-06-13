// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexAuction} from "@contracts/bnb/IntexAuction.sol";
import {EscrowAdapter} from "@contracts/bnb/EscrowAdapter.sol";
import {OriginMessenger} from "@contracts/outbe/OriginMessenger.sol";
import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";

/// @dev Deploys implementation + ERC1967 proxy pairs for the UUPS contracts under test.
///      Returned handles point at the proxy, mirroring how the contracts run in production.
library DeployProxy {
    function intexNFT1155(address defaultAdmin, address bridger) internal returns (IntexNFT1155) {
        IntexNFT1155 impl = new IntexNFT1155();
        ERC1967Proxy proxy =
            new ERC1967Proxy(address(impl), abi.encodeCall(IntexNFT1155.initialize, (defaultAdmin, bridger)));
        return IntexNFT1155(address(proxy));
    }

    function intexAuction(address defaultAdmin, address bridger) internal returns (IntexAuction) {
        IntexAuction impl = new IntexAuction();
        ERC1967Proxy proxy =
            new ERC1967Proxy(address(impl), abi.encodeCall(IntexAuction.initialize, (defaultAdmin, bridger)));
        return IntexAuction(address(proxy));
    }

    function escrowAdapter(address defaultAdmin, address bridger) internal returns (EscrowAdapter) {
        EscrowAdapter impl = new EscrowAdapter();
        ERC1967Proxy proxy =
            new ERC1967Proxy(address(impl), abi.encodeCall(EscrowAdapter.initialize, (defaultAdmin, bridger)));
        return EscrowAdapter(address(proxy));
    }

    function originMessenger(address lzEndpoint, address delegate, uint32 bnbEid) internal returns (OriginMessenger) {
        OriginMessenger impl = new OriginMessenger(lzEndpoint, bnbEid);
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(OriginMessenger.initialize, (delegate)));
        return OriginMessenger(payable(address(proxy)));
    }

    function targetMessenger(address lzEndpoint, address delegate, uint32 outbeEid) internal returns (TargetMessenger) {
        TargetMessenger impl = new TargetMessenger(lzEndpoint, outbeEid);
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(TargetMessenger.initialize, (delegate)));
        return TargetMessenger(payable(address(proxy)));
    }
}
