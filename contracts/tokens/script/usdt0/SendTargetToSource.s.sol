// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

import {ERC7786TokenBridge} from "../../src/ERC7786TokenBridge.sol";

/// @title SendTargetToSource
/// @notice Burn USDT0 on Outbe and unlock USDT on BNB through ERC-7786.
contract SendTargetToSource is Script {
    error InsufficientTokenBalance(address signer, uint256 balance, uint256 required);
    error InsufficientNativeBalance(address signer, uint256 balance, uint256 required);
    error BridgeTokenMismatch(address configuredToken, address bridgeToken);
    error DomainTooLarge(uint256 chainId);

    function _getPrivateKey() internal view returns (uint256) {
        return vm.parseUint(vm.envString("PRIVATE_KEY"));
    }

    function _toDomain(uint256 chainId) internal pure returns (uint32) {
        if (chainId > type(uint32).max) revert DomainTooLarge(chainId);
        return uint32(chainId);
    }

    function run() external returns (bytes32 sendId, uint256 nativeFee) {
        uint256 pk = _getPrivateKey();
        address signer = vm.addr(pk);
        uint32 destinationChainId = _toDomain(vm.envUint("BSC_CHAIN_ID"));
        address recipient = vm.envAddress("RECIPIENT");
        address configuredToken = vm.envAddress("OUTBE_USDT0_TOKEN");
        address tokenBridge = vm.envAddress("OUTBE_USDT0_BRIDGE");
        uint256 amount = vm.envUint("SEND_AMOUNT_LD");

        ERC7786TokenBridge bridge = ERC7786TokenBridge(tokenBridge);
        address bridgeToken = address(bridge.token());
        if (configuredToken != bridgeToken) revert BridgeTokenMismatch(configuredToken, bridgeToken);

        nativeFee = bridge.quoteSend(destinationChainId, recipient, amount, "", 0);

        uint256 tokenBalance = IERC20(configuredToken).balanceOf(signer);
        if (tokenBalance < amount) revert InsufficientTokenBalance(signer, tokenBalance, amount);
        if (signer.balance < nativeFee) revert InsufficientNativeBalance(signer, signer.balance, nativeFee);

        vm.startBroadcast(pk);
        sendId = bridge.send{value: nativeFee}(destinationChainId, recipient, amount);
        vm.stopBroadcast();

        console2.log("Sent USDT0 from Outbe to BNB:");
        console2.logBytes32(sendId);
        console2.log("  Native fee:", nativeFee);
        console2.log("  Amount:", amount);
    }
}
