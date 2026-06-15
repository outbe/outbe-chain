// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {Create2} from "@openzeppelin/contracts/utils/Create2.sol";

import {USDT} from "../../src/USDT.sol";
import {OFTAdapter} from "../../src/OFTAdapter.sol";
import {USDT0OFT} from "../../src/USDT0OFT.sol";

import {EnforcedOptionParam} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oapp/interfaces/IOAppOptionsType3.sol";
import {OptionsBuilder} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oapp/libs/OptionsBuilder.sol";

/// @title OFTDeploy
/// @notice LayerZero OFT deployment and configuration script
contract OFTDeploy is Script {
    using OptionsBuilder for bytes;

    uint256 private constant MIN_OFT_DECIMALS = 6;
    // LayerZero's default shared decimals are 6, and OFTCore computes 10 ** (local - shared).
    uint256 private constant MAX_OFT_DECIMALS = 83;

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
        uint256 decimals_ = vm.envOr("OFT_DECIMALS", uint256(6));
        if (decimals_ < MIN_OFT_DECIMALS || decimals_ > MAX_OFT_DECIMALS) revert InvalidOftDecimals(decimals_);
        // Safe because MAX_OFT_DECIMALS is below uint8 max and was checked above.
        // forge-lint: disable-next-line(unsafe-typecast)
        return uint8(decimals_);
    }

    function _validateOftDecimals(uint8 decimals_) internal pure {
        if (decimals_ < MIN_OFT_DECIMALS || decimals_ > MAX_OFT_DECIMALS) revert InvalidOftDecimals(decimals_);
    }

    function _getOftCreate2Salt() internal view returns (bytes32) {
        return keccak256(bytes(vm.envOr("OFT_CREATE2_SALT", string("USDT0OFT"))));
    }

    function _getOftCreationCode(string memory oftName, string memory oftSymbol, uint8 oftDecimals)
        internal
        returns (bytes memory)
    {
        address owner = vm.envAddress("DEPLOYER_ADDRESS");
        address endpoint = vm.envAddress("LZ_ENDPOINT");
        return
            abi.encodePacked(type(USDT0OFT).creationCode, abi.encode(oftName, oftSymbol, oftDecimals, endpoint, owner));
    }

    function _predictOutbe(string memory oftName, string memory oftSymbol, uint8 oftDecimals)
        internal
        returns (address predicted, bytes32 salt, bytes memory creationCode)
    {
        salt = _getOftCreate2Salt();
        creationCode = _getOftCreationCode(oftName, oftSymbol, oftDecimals);
        predicted = Create2.computeAddress(salt, keccak256(creationCode), CREATE2_FACTORY);
    }

    // ============ Deployment ============

    /// @notice Deploy USDT (mock ERC20) and OFTAdapter on source chain (BSC)
    function deploySource() external returns (address sourceToken, address adapter) {
        uint256 pk = _getPrivateKey();
        address owner = vm.envAddress("DEPLOYER_ADDRESS");
        address endpoint = vm.envAddress("LZ_ENDPOINT");
        uint256 initialMint = vm.envOr("INITIAL_MINT_AMOUNT", uint256(1_000_000_000e6));

        _requireCode(endpoint);

        vm.startBroadcast(pk);
        USDT token = new USDT();
        token.mint(owner, initialMint);
        OFTAdapter oftAdapter = new OFTAdapter(address(token), endpoint, owner);
        vm.stopBroadcast();

        sourceToken = address(token);
        adapter = address(oftAdapter);

        console2.log("USDT0_TOKEN=", sourceToken);
        console2.log("BSC_OFT_ADAPTER=", adapter);
    }

    /// @notice Deploy USDT0OFT on Outbe using OFT_NAME/OFT_SYMBOL/OFT_DECIMALS env vars.
    function deployTarget() external returns (address oftToken) {
        string memory oftName = vm.envOr("OFT_NAME", string("USDT0"));
        string memory oftSymbol = vm.envOr("OFT_SYMBOL", string("USDT0"));
        uint8 oftDecimals = _getOftDecimals();

        oftToken = _deployTarget(oftName, oftSymbol, oftDecimals);
    }

    /// @notice Predict the CREATE2 address for USDT0OFT using env metadata.
    function predictOutbe() external returns (address oftToken) {
        string memory oftName = vm.envOr("OFT_NAME", string("USDT0"));
        string memory oftSymbol = vm.envOr("OFT_SYMBOL", string("USDT0"));
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
            console2.log("USDT0OFT already deployed at predicted address");
        }

        console2.log("USDT0_OFT_TOKEN=", oftToken);
        console2.log("CREATE2_FACTORY=", CREATE2_FACTORY);
        console2.logBytes32(salt);
    }

    function _predictOutbeAndLog(string memory oftName, string memory oftSymbol, uint8 oftDecimals)
        internal
        returns (address oftToken)
    {
        bytes32 salt;
        (oftToken, salt,) = _predictOutbe(oftName, oftSymbol, oftDecimals);
        console2.log("USDT0_OFT_TOKEN=", oftToken);
        console2.log("CREATE2_FACTORY=", CREATE2_FACTORY);
        console2.logBytes32(salt);
    }

    // ============ Peer Configuration ============

    /// @notice Set peer on adapter to point to USDT0OFT on Outbe
    function configureSourcePeer() external {
        uint256 pk = _getPrivateKey();
        uint32 outbeEid = uint32(vm.envUint("OUTBE_EID"));
        address adapter = vm.envAddress("BSC_OFT_ADAPTER");
        address oftToken = vm.envAddress("USDT0_OFT_TOKEN");

        _requireCode(adapter);

        vm.startBroadcast(pk);
        OFTAdapter(adapter).setPeer(outbeEid, _toBytes32(oftToken));
        vm.stopBroadcast();

        console2.log("Source peer configured for EID", outbeEid);
    }

    /// @notice Set peer on USDT0OFT to point to adapter on source chain
    function configureTargetPeer() external {
        uint256 pk = _getPrivateKey();
        uint32 srcEid = uint32(vm.envUint("BSC_EID"));
        address adapter = vm.envAddress("BSC_OFT_ADAPTER");
        address oftToken = vm.envAddress("USDT0_OFT_TOKEN");

        _requireCode(oftToken);

        address signer = vm.addr(pk);
        address owner = USDT0OFT(oftToken).owner();
        if (signer != owner) revert UnauthorizedSigner(signer, owner);

        bytes32 desiredPeer = _toBytes32(adapter);
        if (USDT0OFT(oftToken).peers(srcEid) == desiredPeer) {
            console2.log("Target peer already configured for EID", srcEid);
            return;
        }

        vm.startBroadcast(pk);
        USDT0OFT(oftToken).setPeer(srcEid, desiredPeer);
        vm.stopBroadcast();

        console2.log("Target peer configured for EID", srcEid);
    }

    // ============ Enforced Options ============

    /// @notice Configure enforced options on adapter (source chain)
    function setSourceOptions() external {
        uint256 pk = _getPrivateKey();
        uint32 outbeEid = uint32(vm.envUint("OUTBE_EID"));
        address adapter = vm.envAddress("BSC_OFT_ADAPTER");

        _requireCode(adapter);

        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200_000, 0);
        EnforcedOptionParam[] memory params = new EnforcedOptionParam[](1);
        params[0] = EnforcedOptionParam({eid: outbeEid, msgType: 1, options: options});

        vm.startBroadcast(pk);
        OFTAdapter(adapter).setEnforcedOptions(params);
        vm.stopBroadcast();

        console2.log("Source options configured for EID", outbeEid);
    }

    /// @notice Configure enforced options on USDT0OFT (Outbe chain)
    function setTargetOptions() external {
        uint256 pk = _getPrivateKey();
        uint32 srcEid = uint32(vm.envUint("BSC_EID"));
        address oftToken = vm.envAddress("USDT0_OFT_TOKEN");

        _requireCode(oftToken);

        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200_000, 0);
        EnforcedOptionParam[] memory params = new EnforcedOptionParam[](1);
        params[0] = EnforcedOptionParam({eid: srcEid, msgType: 1, options: options});

        vm.startBroadcast(pk);
        USDT0OFT(oftToken).setEnforcedOptions(params);
        vm.stopBroadcast();

        console2.log("Target options configured for EID", srcEid);
    }
}
