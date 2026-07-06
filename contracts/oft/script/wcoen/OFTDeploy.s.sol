// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Create2} from "@openzeppelin/contracts/utils/Create2.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {ERC7786TokenBridge} from "../../src/ERC7786TokenBridge.sol";
import {WCOEN} from "../../src/WCOEN.sol";
import {WCOENBridgeToken} from "../../src/WCOENOFT.sol";

/// @title WCOENDeploy
/// @notice ERC-7786 / ERC-7802 deployment and configuration script for WCOEN(Outbe) <> WCOEN(BNB).
contract WCOENDeploy is Script {
    struct SourceDeployment {
        address token;
        address tokenBridge;
        bool tokenFromEnv;
        bytes32 tokenSalt;
        bytes32 bridgeSalt;
        bytes tokenCreationCode;
        bytes bridgeCreationCode;
    }

    struct TargetDeployment {
        address token;
        address tokenBridge;
        bytes32 tokenSalt;
        bytes32 bridgeSalt;
        bytes tokenCreationCode;
        bytes bridgeCreationCode;
    }

    error MissingCode(address target);
    error UnauthorizedSigner(address signer, address expectedOwner);
    error InvalidDecimals(uint256 decimals_);
    error DomainTooLarge(uint256 chainId);
    error TokenBridgeMismatch(address currentBridge, address expectedBridge);
    error InvalidRemoteTokenBridge();
    error Create2FactoryDeploymentFailed(bytes32 salt, address expected);

    function _getPrivateKey() internal view returns (uint256) {
        return vm.parseUint(vm.envString("PRIVATE_KEY"));
    }

    function _requireCode(address target) internal view {
        if (target.code.length == 0) revert MissingCode(target);
    }

    function _requireOwner(address signer, address expectedOwner) internal pure {
        if (signer != expectedOwner) revert UnauthorizedSigner(signer, expectedOwner);
    }

    function _toDomain(uint256 chainId) internal pure returns (uint32) {
        if (chainId > type(uint32).max) revert DomainTooLarge(chainId);
        return uint32(chainId);
    }

    function _getDecimals() internal view returns (uint8) {
        uint256 decimals_ = vm.envOr("TOKEN_DECIMALS", uint256(18));
        if (decimals_ != 18) revert InvalidDecimals(decimals_);
        return 18;
    }

    function _getSourceTokenSalt() internal view returns (bytes32) {
        return keccak256(bytes(vm.envOr("WCOEN_TOKEN_CREATE2_SALT", string("OUTBE_WCOEN_TOKEN"))));
    }

    function _getSourceBridgeSalt() internal view returns (bytes32) {
        return keccak256(bytes(vm.envOr("WCOEN_BRIDGE_CREATE2_SALT", string("OUTBE_WCOEN_BRIDGE"))));
    }

    function _getTargetTokenSalt() internal view returns (bytes32) {
        return keccak256(bytes(vm.envOr("TOKEN_CREATE2_SALT", string("BSC_WCOEN_TOKEN"))));
    }

    function _getTargetBridgeSalt() internal view returns (bytes32) {
        return keccak256(bytes(vm.envOr("TOKEN_BRIDGE_CREATE2_SALT", string("BSC_WCOEN_BRIDGE"))));
    }

    function _getSourceBridgeCreationCode(address token_) internal view returns (bytes memory) {
        return abi.encodePacked(
            type(ERC7786TokenBridge).creationCode,
            abi.encode(
                token_,
                vm.envAddress("BRIDGE_ADDRESS"),
                vm.envAddress("DEPLOYER_ADDRESS"),
                ERC7786TokenBridge.TokenBridgeMode.LockUnlock
            )
        );
    }

    function _getTargetTokenCreationCode(string memory name_, string memory symbol_, uint8 decimals_)
        internal
        view
        returns (bytes memory)
    {
        return abi.encodePacked(type(WCOENBridgeToken).creationCode, abi.encode(name_, symbol_, decimals_, vm.envAddress("DEPLOYER_ADDRESS")));
    }

    function _getTargetBridgeCreationCode(address token_) internal view returns (bytes memory) {
        return abi.encodePacked(
            type(ERC7786TokenBridge).creationCode,
            abi.encode(
                token_,
                vm.envAddress("BRIDGE_ADDRESS"),
                vm.envAddress("DEPLOYER_ADDRESS"),
                ERC7786TokenBridge.TokenBridgeMode.BurnMint
            )
        );
    }

    function _predictSource() internal view returns (SourceDeployment memory source) {
        address configuredToken = vm.envOr("OUTBE_WCOEN_TOKEN", address(0));
        source.tokenFromEnv = configuredToken != address(0);

        if (source.tokenFromEnv) {
            source.token = configuredToken;
        } else {
            source.tokenSalt = _getSourceTokenSalt();
            source.tokenCreationCode = type(WCOEN).creationCode;
            source.token = Create2.computeAddress(source.tokenSalt, keccak256(source.tokenCreationCode), CREATE2_FACTORY);
        }

        source.bridgeSalt = _getSourceBridgeSalt();
        source.bridgeCreationCode = _getSourceBridgeCreationCode(source.token);
        source.tokenBridge =
            Create2.computeAddress(source.bridgeSalt, keccak256(source.bridgeCreationCode), CREATE2_FACTORY);
    }

    function _predictTarget(string memory name_, string memory symbol_, uint8 decimals_)
        internal
        view
        returns (TargetDeployment memory target)
    {
        target.tokenSalt = _getTargetTokenSalt();
        target.tokenCreationCode = _getTargetTokenCreationCode(name_, symbol_, decimals_);
        target.token = Create2.computeAddress(target.tokenSalt, keccak256(target.tokenCreationCode), CREATE2_FACTORY);

        target.bridgeSalt = _getTargetBridgeSalt();
        target.bridgeCreationCode = _getTargetBridgeCreationCode(target.token);
        target.tokenBridge =
            Create2.computeAddress(target.bridgeSalt, keccak256(target.bridgeCreationCode), CREATE2_FACTORY);
    }

    function _deployCreate2(bytes32 salt, bytes memory creationCode, address expected) internal {
        if (expected.code.length != 0) return;

        (bool success,) = CREATE2_FACTORY.call(abi.encodePacked(salt, creationCode));
        if (!success || expected.code.length == 0) revert Create2FactoryDeploymentFailed(salt, expected);
    }

    function predictSource() external view returns (address sourceToken, address tokenBridge) {
        SourceDeployment memory source = _predictSource();
        _logSource(source);
        return (source.token, source.tokenBridge);
    }

    function deploySource() external returns (address sourceToken, address tokenBridge) {
        uint256 pk = _getPrivateKey();
        SourceDeployment memory source = _predictSource();

        _requireCode(vm.envAddress("BRIDGE_ADDRESS"));

        vm.startBroadcast(pk);
        if (!source.tokenFromEnv) _deployCreate2(source.tokenSalt, source.tokenCreationCode, source.token);
        _deployCreate2(source.bridgeSalt, source.bridgeCreationCode, source.tokenBridge);
        vm.stopBroadcast();

        _requireCode(source.token);
        _requireCode(source.tokenBridge);
        _logSource(source);
        return (source.token, source.tokenBridge);
    }

    function predictTarget() external view returns (address token, address tokenBridge) {
        TargetDeployment memory target =
            _predictTarget(vm.envOr("TOKEN_NAME", string("WCOEN")), vm.envOr("TOKEN_SYMBOL", string("WCOEN")), _getDecimals());
        _logTarget(target);
        return (target.token, target.tokenBridge);
    }

    function deployTarget() external returns (address token, address tokenBridge) {
        uint256 pk = _getPrivateKey();
        address signer = vm.addr(pk);
        TargetDeployment memory target =
            _predictTarget(vm.envOr("TOKEN_NAME", string("WCOEN")), vm.envOr("TOKEN_SYMBOL", string("WCOEN")), _getDecimals());

        _requireCode(vm.envAddress("BRIDGE_ADDRESS"));

        vm.startBroadcast(pk);
        _deployCreate2(target.tokenSalt, target.tokenCreationCode, target.token);
        _deployCreate2(target.bridgeSalt, target.bridgeCreationCode, target.tokenBridge);
        vm.stopBroadcast();

        _setTokenBridge(pk, signer, target.token, target.tokenBridge);
        _logTarget(target);
        return (target.token, target.tokenBridge);
    }

    function setTargetTokenBridge() external {
        uint256 pk = _getPrivateKey();
        address signer = vm.addr(pk);
        _setTokenBridge(pk, signer, vm.envAddress("BSC_WCOEN_TOKEN"), vm.envAddress("BSC_WCOEN_BRIDGE"));
    }

    function configureSourceRemote() external {
        _configureRemote(
            vm.envAddress("OUTBE_WCOEN_BRIDGE"),
            vm.envUint("BSC_CHAIN_ID"),
            vm.envAddress("BSC_WCOEN_BRIDGE")
        );
    }

    function configureTargetRemote() external {
        _configureRemote(
            vm.envAddress("BSC_WCOEN_BRIDGE"),
            vm.envUint("OUTBE_CHAIN_ID"),
            vm.envAddress("OUTBE_WCOEN_BRIDGE")
        );
    }

    function _setTokenBridge(uint256 pk, address signer, address token, address tokenBridge) internal {
        _requireCode(token);
        _requireCode(tokenBridge);

        WCOENBridgeToken bridgeableToken = WCOENBridgeToken(token);
        address currentBridge = bridgeableToken.tokenBridge();
        if (currentBridge == tokenBridge) return;
        if (currentBridge != address(0)) revert TokenBridgeMismatch(currentBridge, tokenBridge);

        _requireOwner(signer, bridgeableToken.owner());

        vm.startBroadcast(pk);
        bridgeableToken.setTokenBridge(tokenBridge);
        vm.stopBroadcast();

        console2.log("WCOEN token bridge set:", tokenBridge);
    }

    function _configureRemote(address localTokenBridge, uint256 remoteChainId, address remoteTokenBridge) internal {
        uint256 pk = _getPrivateKey();
        address signer = vm.addr(pk);

        _requireCode(localTokenBridge);
        if (remoteTokenBridge == address(0)) revert InvalidRemoteTokenBridge();
        _requireOwner(signer, Ownable(localTokenBridge).owner());

        uint32 remoteDomain = _toDomain(remoteChainId);
        bytes memory remoteInterop = InteroperableAddress.formatEvmV1(remoteChainId, remoteTokenBridge);

        vm.startBroadcast(pk);
        ERC7786TokenBridge(localTokenBridge).setRemoteBridge(remoteDomain, remoteInterop);
        vm.stopBroadcast();

        console2.log("Remote bridge configured:");
        console2.log("  local=", localTokenBridge);
        console2.log("  remote chainId=", remoteChainId);
        console2.log("  remote bridge=", remoteTokenBridge);
    }

    function _logSource(SourceDeployment memory source) internal pure {
        console2.log("OUTBE_WCOEN_TOKEN=", source.token);
        console2.log("OUTBE_WCOEN_BRIDGE=", source.tokenBridge);
        console2.log("CREATE2_FACTORY=", CREATE2_FACTORY);
        if (source.tokenFromEnv) {
            console2.log("OUTBE_WCOEN_TOKEN provided by env; token salt not used");
        } else {
            console2.log("WCOEN_TOKEN_CREATE2_SALT=");
            console2.logBytes32(source.tokenSalt);
        }
        console2.log("WCOEN_BRIDGE_CREATE2_SALT=");
        console2.logBytes32(source.bridgeSalt);
    }

    function _logTarget(TargetDeployment memory target) internal pure {
        console2.log("BSC_WCOEN_TOKEN=", target.token);
        console2.log("BSC_WCOEN_BRIDGE=", target.tokenBridge);
        console2.log("CREATE2_FACTORY=", CREATE2_FACTORY);
        console2.log("TOKEN_CREATE2_SALT=");
        console2.logBytes32(target.tokenSalt);
        console2.log("TOKEN_BRIDGE_CREATE2_SALT=");
        console2.logBytes32(target.bridgeSalt);
    }
}
