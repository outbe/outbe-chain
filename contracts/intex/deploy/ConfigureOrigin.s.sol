// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {IDesis} from "@contracts/origin/interfaces/IDesis.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";

/// @title ConfigureOrigin
/// @author Outbe
/// @notice Wires the OriginRouter and the NFT collection on Outbe to the Desis, IntexFactory and
///         GemFactory precompiles and to the system caller. Run it after DeployOrigin and
///         DeployTarget, before HandOver. A step that already holds is skipped, so a re-run sends
///         nothing.
/// @dev Env: DEPLOYER_PK, CREATE3_FACTORY_ADDRESS, CONTRACT_SALT (optional), DESIS_ADDRESS,
///      INTEX_FACTORY_ADDRESS, GEM_FACTORY_ADDRESS, OUTBE_SYSTEM_TX_ADDRESS. Needs
///      `--skip-simulation`: the Desis answer below exists only on this script's fork.
contract ConfigureOrigin is BaseScript {
    struct Origin {
        address router;
        address nft;
        address desis;
        address intexFactory;
        address gemFactory;
        address system;
    }

    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PK");
        address deployer = vm.addr(pk);
        Create3Factory factory = create3Factory();
        Origin memory o = Origin({
            router: predictProxy(factory, deployer, "OriginRouter"),
            nft: predictProxy(factory, deployer, "IntexNFT1155"),
            desis: vm.envAddress("DESIS_ADDRESS"),
            intexFactory: vm.envAddress("INTEX_FACTORY_ADDRESS"),
            gemFactory: vm.envAddress("GEM_FACTORY_ADDRESS"),
            system: vm.envAddress("OUTBE_SYSTEM_TX_ADDRESS")
        });
        _mirrorDesisInterface(o.desis);

        vm.startBroadcast(pk);
        configure(deployer, o);
        vm.stopBroadcast();
    }

    /// @dev Requires an active broadcast from `deployer`.
    function configure(address deployer, Origin memory o) public {
        requireDeployed(o.router);
        requireDeployed(o.nft);

        OriginRouter router = OriginRouter(payable(o.router));
        if (router.desis() != o.desis || router.intexFactory() != o.intexFactory) {
            requireDeployerAdmin(o.router, deployer);
            router.wire(o.desis, o.intexFactory);
            console.log("OriginRouter wired");
        }
        grantRoleIfMissing(o.router, router.DESIS_ROLE(), o.system, deployer);
        grantRoleIfMissing(o.router, router.INTEX_FACTORY_ROLE(), o.system, deployer);
        grantRoleIfMissing(o.router, router.INTEX_FACTORY_ROLE(), o.desis, deployer);

        IntexNFT1155 nft = IntexNFT1155(o.nft);
        grantRoleIfMissing(o.nft, nft.RELAYER_ROLE(), o.system, deployer);
        grantRoleIfMissing(o.nft, nft.RELAYER_ROLE(), o.desis, deployer);
        grantRoleIfMissing(o.nft, nft.RELAYER_ROLE(), o.intexFactory, deployer);
        grantRoleIfMissing(o.nft, nft.BURNER_ROLE(), o.intexFactory, deployer);
        grantRoleIfMissing(o.nft, nft.BURNER_ROLE(), o.gemFactory, deployer);
    }

    /// @dev A fork cannot run the Desis precompile, so `OriginRouter.wire` gets the node's answer to
    ///      its interface check. `virtual` so a test, which has no node, can skip it.
    function _mirrorDesisInterface(address desis) internal virtual {
        bytes memory data = abi.encodeCall(IERC165.supportsInterface, (type(IDesis).interfaceId));
        string memory params =
            string.concat('[{"to":"', vm.toString(desis), '","data":"', vm.toString(data), '"},"latest"]');
        vm.mockCall(desis, data, vm.rpc("eth_call", params));
    }
}
