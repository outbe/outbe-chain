// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {Create2} from "@openzeppelin/contracts/utils/Create2.sol";

import {WCOEN} from "../../src/WCOEN.sol";
import {OFTAdapter} from "../../src/OFTAdapter.sol";
import {WCOENOFT} from "../../src/WCOENOFT.sol";

import {EnforcedOptionParam} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oapp/interfaces/IOAppOptionsType3.sol";
import {OptionsBuilder} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oapp/libs/OptionsBuilder.sol";

/// @title OFTDeploy
/// @notice LayerZero OFT deployment and configuration script
contract OFTDeploy is Script {
    using OptionsBuilder for bytes;

    uint8 private constant WCOEN_DECIMALS = 18;

    struct SourceDeployment {
        address token;
        bytes32 tokenSalt;
        bytes tokenCreationCode;
        bool tokenFromEnv;
        address adapter;
        bytes32 adapterSalt;
        bytes adapterCreationCode;
    }

    error MissingCode(address target);
    error UnauthorizedSigner(address signer, address expectedOwner);
    error InvalidOftDecimals(uint256 decimals_);
    error Create2FactoryDeploymentFailed(bytes32 salt, address expected);

    // ============ Helpers ============

    function _getPrivateKey() internal view returns (uint256) {
        string memory key = vm.envString("PRIVATE_KEY");
        return vm.parseUint(key);
    }

    function _toBytes32(address addr) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(addr)));
    }

    function _requireCode(address target) internal view {
        if (target.code.length == 0) revert MissingCode(target);
    }

    function _getOftDecimals() internal view returns (uint8) {
        uint256 decimals_ = vm.envOr("OFT_DECIMALS", uint256(18));
        if (decimals_ != WCOEN_DECIMALS) revert InvalidOftDecimals(decimals_);
        return WCOEN_DECIMALS;
    }

    function _validateOftDecimals(uint8 decimals_) internal pure {
        if (decimals_ != WCOEN_DECIMALS) revert InvalidOftDecimals(decimals_);
    }

    function _getOftCreate2Salt() internal view returns (bytes32) {
        return keccak256(bytes(vm.envOr("OFT_CREATE2_SALT", string("WCOENOFT"))));
    }

    function _getWcoenCreate2Salt() internal view returns (bytes32) {
        return keccak256(bytes(vm.envOr("WCOEN_CREATE2_SALT", string("WCOEN"))));
    }

    function _getAdapterCreate2Salt() internal view returns (bytes32) {
        return keccak256(bytes(vm.envOr("WCOEN_ADAPTER_CREATE2_SALT", string("WCOEN_OFT_ADAPTER"))));
    }

    function _getWcoenCreationCode() internal view returns (bytes memory) {
        return type(WCOEN).creationCode;
    }

    function _getAdapterCreationCode(address token) internal view returns (bytes memory) {
        address owner = vm.envAddress("DEPLOYER_ADDRESS");
        address endpoint = vm.envAddress("LZ_ENDPOINT");
        return abi.encodePacked(type(OFTAdapter).creationCode, abi.encode(token, endpoint, owner));
    }

    function _getOftCreationCode(string memory oftName, string memory oftSymbol, uint8 oftDecimals)
        internal
        view
        returns (bytes memory)
    {
        address owner = vm.envAddress("DEPLOYER_ADDRESS");
        address endpoint = vm.envAddress("LZ_ENDPOINT");
        return
            abi.encodePacked(type(WCOENOFT).creationCode, abi.encode(oftName, oftSymbol, oftDecimals, endpoint, owner));
    }

    function _predictOutbe(string memory oftName, string memory oftSymbol, uint8 oftDecimals)
        internal
        view
        returns (address predicted, bytes32 salt, bytes memory creationCode)
    {
        salt = _getOftCreate2Salt();
        creationCode = _getOftCreationCode(oftName, oftSymbol, oftDecimals);
        predicted = Create2.computeAddress(salt, keccak256(creationCode), CREATE2_FACTORY);
    }

    function _predictSource() internal view returns (SourceDeployment memory source) {
        address configuredToken = vm.envOr("WCOEN_TOKEN", address(0));
        if (configuredToken == address(0)) {
            source.tokenSalt = _getWcoenCreate2Salt();
            source.tokenCreationCode = _getWcoenCreationCode();
            source.token =
                Create2.computeAddress(source.tokenSalt, keccak256(source.tokenCreationCode), CREATE2_FACTORY);
        } else {
            source.token = configuredToken;
            source.tokenFromEnv = true;
        }

        source.adapterSalt = _getAdapterCreate2Salt();
        source.adapterCreationCode = _getAdapterCreationCode(source.token);
        source.adapter =
            Create2.computeAddress(source.adapterSalt, keccak256(source.adapterCreationCode), CREATE2_FACTORY);
    }

    function _deployCreate2(bytes32 salt, bytes memory creationCode, address expected) internal {
        if (expected.code.length != 0) return;

        (bool success,) = CREATE2_FACTORY.call(abi.encodePacked(salt, creationCode));
        if (!success || expected.code.length == 0) revert Create2FactoryDeploymentFailed(salt, expected);
    }

    function _logSource(SourceDeployment memory source) internal pure {
        console2.log("WCOEN_TOKEN=", source.token);
        console2.log("OUTBE_OFT_ADAPTER=", source.adapter);
        console2.log("CREATE2_FACTORY=", CREATE2_FACTORY);
        if (source.tokenFromEnv) {
            console2.log("WCOEN_TOKEN provided by env; WCOEN CREATE2 salt not used");
        } else {
            console2.log("WCOEN_CREATE2_SALT=");
            console2.logBytes32(source.tokenSalt);
        }
        console2.log("WCOEN_ADAPTER_CREATE2_SALT=");
        console2.logBytes32(source.adapterSalt);
    }

    // ============ Deployment ============

    /// @notice Deploy or reuse source-side WCOEN and Outbe OFTAdapter at CREATE2 addresses.
    function deploySource() external returns (address sourceToken, address adapter) {
        uint256 pk = _getPrivateKey();
        address endpoint = vm.envAddress("LZ_ENDPOINT");
        SourceDeployment memory source = _predictSource();

        _requireCode(endpoint);
        if (source.tokenFromEnv) {
            _requireCode(source.token);
        }

        if (!source.tokenFromEnv && source.token.code.length == 0) {
            vm.startBroadcast(pk);
            _deployCreate2(source.tokenSalt, source.tokenCreationCode, source.token);
            vm.stopBroadcast();
        } else if (!source.tokenFromEnv) {
            console2.log("WCOEN already deployed at predicted address");
        }

        if (source.adapter.code.length == 0) {
            vm.startBroadcast(pk);
            _deployCreate2(source.adapterSalt, source.adapterCreationCode, source.adapter);
            vm.stopBroadcast();
        } else {
            console2.log("OFTAdapter already deployed at predicted address");
        }

        sourceToken = source.token;
        adapter = source.adapter;

        _logSource(source);
    }

    /// @notice Predict CREATE2 addresses for source-side WCOEN and Outbe OFTAdapter.
    function predictSource() external view returns (address sourceToken, address adapter) {
        SourceDeployment memory source = _predictSource();
        sourceToken = source.token;
        adapter = source.adapter;

        _logSource(source);
    }

    /// @notice Deploy WCOENOFT on BSC using OFT_NAME/OFT_SYMBOL/OFT_DECIMALS env vars.
    function deployTarget() external returns (address oftToken) {
        string memory oftName = vm.envOr("OFT_NAME", string("WCOEN"));
        string memory oftSymbol = vm.envOr("OFT_SYMBOL", string("WCOEN"));
        uint8 oftDecimals = _getOftDecimals();

        oftToken = _deployTarget(oftName, oftSymbol, oftDecimals);
    }

    /// @notice Predict the CREATE2 address for WCOENOFT on BSC using env metadata.
    function predictOutbe() external view returns (address oftToken) {
        string memory oftName = vm.envOr("OFT_NAME", string("WCOEN"));
        string memory oftSymbol = vm.envOr("OFT_SYMBOL", string("WCOEN"));
        uint8 oftDecimals = _getOftDecimals();

        oftToken = _predictOutbeAndLog(oftName, oftSymbol, oftDecimals);
    }

    function _deployTarget(string memory oftName, string memory oftSymbol, uint8 oftDecimals)
        internal
        returns (address oftToken)
    {
        uint256 pk = _getPrivateKey();
        address endpoint = vm.envAddress("LZ_ENDPOINT");

        _requireCode(endpoint);

        (address predicted, bytes32 salt, bytes memory creationCode) = _predictOutbe(oftName, oftSymbol, oftDecimals);
        oftToken = predicted;

        if (oftToken.code.length == 0) {
            vm.startBroadcast(pk);
            (bool success,) = CREATE2_FACTORY.call(abi.encodePacked(salt, creationCode));
            vm.stopBroadcast();

            if (!success || oftToken.code.length == 0) revert Create2FactoryDeploymentFailed(salt, oftToken);
        } else {
            console2.log("WCOENOFT already deployed at predicted address");
        }

        console2.log("WCOEN_OFT_TOKEN=", oftToken);
        console2.log("CREATE2_FACTORY=", CREATE2_FACTORY);
        console2.logBytes32(salt);
        console2.log("OFT_NAME=", oftName);
        console2.log("OFT_SYMBOL=", oftSymbol);
        console2.log("OFT_DECIMALS=", oftDecimals);
    }

    function _predictOutbeAndLog(string memory oftName, string memory oftSymbol, uint8 oftDecimals)
        internal
        view
        returns (address oftToken)
    {
        bytes32 salt;
        (oftToken, salt,) = _predictOutbe(oftName, oftSymbol, oftDecimals);
        console2.log("WCOEN_OFT_TOKEN=", oftToken);
        console2.log("CREATE2_FACTORY=", CREATE2_FACTORY);
        console2.logBytes32(salt);
    }

    // ============ Peer Configuration ============

    /// @notice Set peer on adapter to point to WCOENOFT on Outbe
    function configureSourcePeer() external {
        uint256 pk = _getPrivateKey();
        uint32 bscEid = uint32(vm.envUint("BSC_EID"));
        address adapter = vm.envAddress("OUTBE_OFT_ADAPTER");
        address oftToken = vm.envAddress("WCOEN_OFT_TOKEN");

        _requireCode(adapter);

        vm.startBroadcast(pk);
        OFTAdapter(adapter).setPeer(bscEid, _toBytes32(oftToken));
        vm.stopBroadcast();

        console2.log("Source peer configured for EID", bscEid);
    }

    /// @notice Set peer on WCOENOFT to point to adapter on source chain
    function configureTargetPeer() external {
        uint256 pk = _getPrivateKey();
        uint32 outbeEid = uint32(vm.envUint("OUTBE_EID"));
        address adapter = vm.envAddress("OUTBE_OFT_ADAPTER");
        address oftToken = vm.envAddress("WCOEN_OFT_TOKEN");

        _requireCode(oftToken);

        address signer = vm.addr(pk);
        address owner = WCOENOFT(oftToken).owner();
        if (signer != owner) revert UnauthorizedSigner(signer, owner);

        bytes32 desiredPeer = _toBytes32(adapter);
        if (WCOENOFT(oftToken).peers(outbeEid) == desiredPeer) {
            console2.log("Target peer already configured for EID", outbeEid);
            return;
        }

        vm.startBroadcast(pk);
        WCOENOFT(oftToken).setPeer(outbeEid, desiredPeer);
        vm.stopBroadcast();

        console2.log("Target peer configured for EID", outbeEid);
    }

    // ============ Enforced Options ============

    /// @notice Configure enforced options on adapter (source chain)
    function setSourceOptions() external {
        uint256 pk = _getPrivateKey();
        uint32 bscEid = uint32(vm.envUint("BSC_EID"));
        address adapter = vm.envAddress("OUTBE_OFT_ADAPTER");

        _requireCode(adapter);

        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200_000, 0);
        EnforcedOptionParam[] memory params = new EnforcedOptionParam[](1);
        params[0] = EnforcedOptionParam({eid: bscEid, msgType: 1, options: options});

        vm.startBroadcast(pk);
        OFTAdapter(adapter).setEnforcedOptions(params);
        vm.stopBroadcast();

        console2.log("Source options configured for EID", bscEid);
    }

    /// @notice Configure enforced options on WCOENOFT (BSC chain)
    function setTargetOptions() external {
        uint256 pk = _getPrivateKey();
        uint32 outbeEid = uint32(vm.envUint("OUTBE_EID"));
        address oftToken = vm.envAddress("WCOEN_OFT_TOKEN");

        _requireCode(oftToken);

        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200_000, 0);
        EnforcedOptionParam[] memory params = new EnforcedOptionParam[](1);
        params[0] = EnforcedOptionParam({eid: outbeEid, msgType: 1, options: options});

        vm.startBroadcast(pk);
        WCOENOFT(oftToken).setEnforcedOptions(params);
        vm.stopBroadcast();

        console2.log("Target options configured for EID", outbeEid);
    }
}
