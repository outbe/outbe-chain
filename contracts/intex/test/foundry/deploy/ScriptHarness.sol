// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {DeployAll} from "../../../script/DeployAll.s.sol";
import {HandOver} from "../../../script/6_HandOver.s.sol";
import {PrepareUpgrade} from "../../../script/PrepareUpgrade.s.sol";

/// @dev The env of a script, held by the script itself, so parallel tests share no env value. A test
///      has no Outbe node, so it also names the Interchain Account of Outbe.
abstract contract ScriptEnv {
    address internal factory;
    string internal salt;
    bool internal outbe;
    address internal outbeIca;

    function setEnv(address factory_, string memory salt_, bool outbe_, address outbeIca_) external {
        factory = factory_;
        salt = salt_;
        outbe = outbe_;
        outbeIca = outbeIca_;
    }
}

contract DeployAllHarness is DeployAll, ScriptEnv {
    function create3Factory() public view override returns (address) {
        return factory;
    }

    function saltVersion() public view override returns (string memory) {
        return salt;
    }

    function onOutbe() public view override returns (bool) {
        return outbe;
    }

    function _remoteAccount(uint32) internal view override returns (address) {
        return outbeIca;
    }

    /// @dev What `run` does after it reads the env, with the key's account as the deployer.
    function deployWith(uint256 pk, Config memory c) external {
        address deployer = vm.addr(pk);
        vm.startBroadcast(pk);
        deployAll(deployer, stack(deployer), c);
        vm.stopBroadcast();
        checkLinks(stack(deployer), c);
    }
}

contract HandOverHarness is HandOver, ScriptEnv {
    function create3Factory() public view override returns (address) {
        return factory;
    }

    function saltVersion() public view override returns (string memory) {
        return salt;
    }

    function onOutbe() public view override returns (bool) {
        return outbe;
    }

    function _remoteAccount(uint32) internal view override returns (address) {
        return outbeIca;
    }

    function handOverWith(uint256 pk) external {
        address governance = governanceOwner();
        vm.startBroadcast(pk);
        handOver(governance, stack(vm.addr(pk)));
        vm.stopBroadcast();
    }
}

contract PrepareUpgradeHarness is PrepareUpgrade, ScriptEnv {
    function create3Factory() public view override returns (address) {
        return factory;
    }

    function saltVersion() public view override returns (string memory) {
        return salt;
    }

    function onOutbe() public view override returns (bool) {
        return outbe;
    }

    function _remoteAccount(uint32) internal view override returns (address) {
        return outbeIca;
    }

    string internal commit;

    function setSourceCommit(string memory commit_) external {
        commit = commit_;
    }

    function _sourceCommit() internal view override returns (string memory) {
        return commit;
    }

    function _executionDelayBlocks() internal pure override returns (uint256) {
        return 50;
    }
}
