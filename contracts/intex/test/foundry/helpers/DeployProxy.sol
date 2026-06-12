// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";

/// @dev Deploys implementation + ERC1967 proxy pairs for the UUPS contracts under test.
///      Returned handles point at the proxy, mirroring how the contracts run in production.
library DeployProxy {
    function intexNFT1155(address defaultAdmin, address bridger) internal returns (IntexNFT1155) {
        IntexNFT1155 impl = new IntexNFT1155();
        ERC1967Proxy proxy =
            new ERC1967Proxy(address(impl), abi.encodeCall(IntexNFT1155.initialize, (defaultAdmin, bridger)));
        return IntexNFT1155(address(proxy));
    }
}
