// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {WCOEN} from "../../src/WCOEN.sol";
import {OFTAdapter} from "../../src/OFTAdapter.sol";

import {SendParam, OFTReceipt} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oft/interfaces/IOFT.sol";
import {
    MessagingFee,
    MessagingReceipt
} from "@layerzerolabs/lz-evm-protocol-v2/contracts/interfaces/ILayerZeroEndpointV2.sol";

/// @title SendSourceToTarget
/// @notice Lock WCOEN on Outbe and mint WCOENOFT on BSC.
contract SendSourceToTarget is Script {
    error InsufficientTokenBalance(address signer, uint256 balance, uint256 required);
    error InsufficientNativeBalance(address signer, uint256 balance, uint256 required);

    function _getPrivateKey() internal view returns (uint256) {
        string memory key = vm.envString("PRIVATE_KEY");
        return vm.parseUint(key);
    }

    function _toBytes32(address addr) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(addr)));
    }

    /// @notice Send WCOEN from Outbe to BSC.
    function run() external returns (bytes32 guid, uint64 nonce, uint256 nativeFee) {
        uint256 pk = _getPrivateKey();
        address signer = vm.addr(pk);
        uint32 outbeEid = uint32(vm.envUint("BSC_EID"));
        address deployer = vm.envAddress("DEPLOYER_ADDRESS");
        address recipient = vm.envAddress("RECIPIENT");
        address sourceToken = vm.envAddress("WCOEN_TOKEN");
        address adapter = vm.envAddress("OUTBE_OFT_ADAPTER");
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

        MessagingFee memory fee = OFTAdapter(adapter).quoteSend(sp, false);

        require(WCOEN(payable(sourceToken)).balanceOf(signer) >= amount, "insufficient token balance");
        require(signer.balance >= fee.nativeFee, "insufficient native balance");

        vm.startBroadcast(pk);
        WCOEN(payable(sourceToken)).approve(adapter, amount);
        (MessagingReceipt memory receipt, OFTReceipt memory oftReceipt) = OFTAdapter(adapter)
        .send{value: fee.nativeFee}(
            sp, MessagingFee({nativeFee: fee.nativeFee, lzTokenFee: 0}), deployer
        );
        vm.stopBroadcast();

        guid = receipt.guid;
        nonce = receipt.nonce;
        nativeFee = fee.nativeFee;

        console2.log("Sent from Outbe to BSC:");
        console2.logBytes32(guid);
        console2.log("  Nonce:", nonce);
        console2.log("  Amount sent:", oftReceipt.amountSentLD);
        console2.log("  Amount received:", oftReceipt.amountReceivedLD);
    }
}
