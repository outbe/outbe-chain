// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Create3Factory} from "@shared/Create3Factory.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";

/// @title ConfigureOrigin
/// @author Outbe
/// @notice Gives the IntexFactory and GemFactory precompiles the burner role of the NFT collection
///         on Outbe. Run it after DeployTarget, before HandOver. A step that already holds is
///         skipped, so a re-run sends nothing.
/// @dev Env: DEPLOYER_PK, CREATE3_FACTORY_ADDRESS, CONTRACT_SALT (optional), INTEX_FACTORY_ADDRESS,
///      GEM_FACTORY_ADDRESS.
contract ConfigureOrigin is BaseScript {
    struct Origin {
        address nft;
        address intexFactory;
        address gemFactory;
    }

    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PK");
        address deployer = vm.addr(pk);
        Create3Factory factory = create3Factory();
        Origin memory o = Origin({
            nft: predictProxy(factory, deployer, "IntexNFT1155"),
            intexFactory: vm.envAddress("INTEX_FACTORY_ADDRESS"),
            gemFactory: vm.envAddress("GEM_FACTORY_ADDRESS")
        });

        vm.startBroadcast(pk);
        configure(deployer, o);
        vm.stopBroadcast();
    }

    /// @dev Requires an active broadcast from `deployer`.
    function configure(address deployer, Origin memory o) public {
        requireDeployed(o.nft);
        IntexNFT1155 nft = IntexNFT1155(o.nft);
        grantRoleIfMissing(o.nft, nft.BURNER_ROLE(), o.intexFactory, deployer);
        grantRoleIfMissing(o.nft, nft.BURNER_ROLE(), o.gemFactory, deployer);
    }
}
