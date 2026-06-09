// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {WCOENOFT} from "../../src/WCOENOFT.sol";

import {SendParam, OFTReceipt} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oft/interfaces/IOFT.sol";
import {
    MessagingFee,
    MessagingReceipt
} from "@layerzerolabs/lz-evm-protocol-v2/contracts/interfaces/ILayerZeroEndpointV2.sol";

/// @title SendTargetToSource
/// @notice Burn WCOENOFT on BSC and unlock WCOEN on Outbe.
contract SendTargetToSource is Script {
    error InsufficientTokenBalance(address signer, uint256 balance, uint256 required);
    error InsufficientNativeBalance(address signer, uint256 balance, uint256 required);

    function _getPrivateKey() internal view returns (uint256) {
        string memory key = vm.envString("PRIVATE_KEY");
        return vm.parseUint(key);
    }

    function _toBytes32(address addr) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(addr)));
    }

    /// @notice Send WCOENOFT from BSC to Outbe.
    function run() external returns (bytes32 guid, uint64 nonce, uint256 nativeFee) {
        uint256 pk = _getPrivateKey();
        address signer = vm.addr(pk);
        uint32 outbeEid = uint32(vm.envUint("OUTBE_EID"));
        address deployer = vm.envAddress("DEPLOYER_ADDRESS");
        address recipient = vm.envAddress("RECIPIENT");
        address oftToken = vm.envAddress("WCOEN_OFT_TOKEN");
        uint256 amount = vm.envUint("SEND_AMOUNT_LD");
        uint256 minAmount = vm.envOr("SEND_MIN_AMOUNT_LD", amount);

        SendParam memory sp = SendParam({
            dstEid: outbeEid,
            to: _toBytes32(recipient),
            amountLD: amount,
            minAmountLD: minAmount,
            extraOptions: bytes(""),
            composeMsg: bytes(""),
            oftCmd: bytes("")
        });

        MessagingFee memory fee = WCOENOFT(oftToken).quoteSend(sp, false);

        require(WCOENOFT(oftToken).balanceOf(signer) >= amount, "insufficient token balance");
        require(signer.balance >= fee.nativeFee, "insufficient native balance");

        vm.startBroadcast(pk);
        (MessagingReceipt memory receipt, OFTReceipt memory oftReceipt) = WCOENOFT(oftToken).send{value: fee.nativeFee}(
            sp, MessagingFee({nativeFee: fee.nativeFee, lzTokenFee: 0}), deployer
        );
        vm.stopBroadcast();

        guid = receipt.guid;
        nonce = receipt.nonce;
        nativeFee = fee.nativeFee;

        console2.log("Sent from BSC to Outbe:");
        console2.logBytes32(guid);
        console2.log("  Nonce:", nonce);
        console2.log("  Amount sent:", oftReceipt.amountSentLD);
        console2.log("  Amount received:", oftReceipt.amountReceivedLD);
    }
}
