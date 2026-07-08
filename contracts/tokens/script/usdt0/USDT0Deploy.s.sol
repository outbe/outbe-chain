// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Create2} from "@openzeppelin/contracts/utils/Create2.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {ERC7786TokenBridge} from "../../src/ERC7786TokenBridge.sol";
import {USDT} from "../../src/native/USDT.sol";
import {BridgeableERC20Stable} from "../../src/synthetic/BridgeableERC20Stable.sol";

/// @title USDT0Deploy
/// @notice ERC-7786 / ERC-7802 deployment and configuration script for USDT(BNB) <> USDT0(Outbe).
contract USDT0Deploy is Script {
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
    error InvalidIsoCode(uint256 isoCode_);
    error DomainTooLarge(uint256 chainId);
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
        uint256 decimals_ = vm.envOr("TOKEN_DECIMALS", uint256(6));
        if (decimals_ > type(uint8).max) revert InvalidDecimals(decimals_);
        return uint8(decimals_);
    }

    function _getIsoCode() internal view returns (uint16) {
        uint256 isoCode_ = vm.envOr("TOKEN_ISO_CODE", uint256(840));
        if (isoCode_ > type(uint16).max) revert InvalidIsoCode(isoCode_);
        return uint16(isoCode_);
    }

    function _getTokenSalt() internal pure returns (bytes32) {
        return keccak256(bytes(string("USDT0")));
    }

    function _getBridgeSalt() internal pure returns (bytes32) {
        return keccak256(bytes(string("USDT0_BRIDGE")));
    }

    function _getTokenCreationCode(string memory name_, string memory symbol_, uint8 decimals_)
        internal
        view
        returns (bytes memory)
    {
        return abi.encodePacked(
            type(BridgeableERC20Stable).creationCode,
            abi.encode(name_, symbol_, decimals_, _getIsoCode(), vm.envAddress("DEPLOYER_ADDRESS"))
        );
    }

    function _getBridgeCreationCode(address token_) internal view returns (bytes memory) {
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

    function _predictTarget(string memory name_, string memory symbol_, uint8 decimals_)
        internal
        view
        returns (TargetDeployment memory target)
    {
        target.tokenSalt = _getTokenSalt();
        target.tokenCreationCode = _getTokenCreationCode(name_, symbol_, decimals_);
        target.token = Create2.computeAddress(target.tokenSalt, keccak256(target.tokenCreationCode), CREATE2_FACTORY);

        target.bridgeSalt = _getBridgeSalt();
        target.bridgeCreationCode = _getBridgeCreationCode(target.token);
        target.tokenBridge =
            Create2.computeAddress(target.bridgeSalt, keccak256(target.bridgeCreationCode), CREATE2_FACTORY);
    }

    function _deployCreate2(bytes32 salt, bytes memory creationCode, address expected) internal {
        if (expected.code.length != 0) return;

        (bool success,) = CREATE2_FACTORY.call(abi.encodePacked(salt, creationCode));
        if (!success || expected.code.length == 0) revert Create2FactoryDeploymentFailed(salt, expected);
    }

    function deploySource() external returns (address sourceToken, address tokenBridge) {
        uint256 pk = _getPrivateKey();
        address owner = vm.envAddress("DEPLOYER_ADDRESS");
        address localBridge = vm.envAddress("BRIDGE_ADDRESS");
        address configuredToken = vm.envOr("BSC_USDT_TOKEN", address(0));
        address configuredBridge = vm.envOr("BSC_USDT_BRIDGE", address(0));
        uint256 initialMint = vm.envOr("INITIAL_MINT_AMOUNT", uint256(1_000_000_000e6));

        _requireCode(localBridge);

        vm.startBroadcast(pk);
        if (configuredToken == address(0)) {
            USDT token = new USDT();
            token.mint(owner, initialMint);
            sourceToken = address(token);
        } else {
            sourceToken = configuredToken;
        }

        if (configuredBridge == address(0)) {
            tokenBridge = address(
                new ERC7786TokenBridge(sourceToken, localBridge, owner, ERC7786TokenBridge.TokenBridgeMode.LockUnlock)
            );
        } else {
            tokenBridge = configuredBridge;
        }
        vm.stopBroadcast();

        _requireCode(sourceToken);
        _requireCode(tokenBridge);

        console2.log("BSC_USDT_TOKEN=", sourceToken);
        console2.log("BSC_USDT_BRIDGE=", tokenBridge);
    }

    function predictOutbe() external view returns (address token, address tokenBridge) {
        TargetDeployment memory target = _predictTarget(
            vm.envOr("TOKEN_NAME", string("USDT0")), vm.envOr("TOKEN_SYMBOL", string("USDT0")), _getDecimals()
        );
        _logTarget(target);
        return (target.token, target.tokenBridge);
    }

    function deployTarget() external returns (address token, address tokenBridge) {
        uint256 pk = _getPrivateKey();
        address signer = vm.addr(pk);
        TargetDeployment memory target = _predictTarget(
            vm.envOr("TOKEN_NAME", string("USDT0")), vm.envOr("TOKEN_SYMBOL", string("USDT0")), _getDecimals()
        );

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
        _setTokenBridge(pk, signer, vm.envAddress("OUTBE_USDT0_TOKEN"), vm.envAddress("OUTBE_USDT0_BRIDGE"));
    }

    function configureSourceRemote() external {
        _configureRemote(
            vm.envAddress("BSC_USDT_BRIDGE"), vm.envUint("OUTBE_CHAIN_ID"), vm.envAddress("OUTBE_USDT0_BRIDGE")
        );
    }

    function configureTargetRemote() external {
        _configureRemote(
            vm.envAddress("OUTBE_USDT0_BRIDGE"), vm.envUint("BSC_CHAIN_ID"), vm.envAddress("BSC_USDT_BRIDGE")
        );
    }

    function _setTokenBridge(uint256 pk, address signer, address token, address tokenBridge) internal {
        _requireCode(token);
        _requireCode(tokenBridge);

        BridgeableERC20Stable bridgeableToken = BridgeableERC20Stable(token);
        address currentBridge = bridgeableToken.tokenBridge();
        if (currentBridge == tokenBridge) return;

        _requireOwner(signer, bridgeableToken.owner());

        vm.startBroadcast(pk);
        bridgeableToken.setTokenBridge(tokenBridge);
        vm.stopBroadcast();

        console2.log("USDT0 token bridge set:", tokenBridge);
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

    function _logTarget(TargetDeployment memory target) internal pure {
        console2.log("OUTBE_USDT0_TOKEN=", target.token);
        console2.log("OUTBE_USDT0_BRIDGE=", target.tokenBridge);
        console2.log("CREATE2_FACTORY=", CREATE2_FACTORY);
        console2.log("TOKEN_CREATE2_SALT=");
        console2.logBytes32(target.tokenSalt);
        console2.log("TOKEN_BRIDGE_CREATE2_SALT=");
        console2.logBytes32(target.bridgeSalt);
    }
}
