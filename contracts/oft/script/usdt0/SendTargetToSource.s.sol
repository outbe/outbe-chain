// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {USDT0OFT} from "../../src/USDT0OFT.sol";

import {SendParam, OFTReceipt} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oft/interfaces/IOFT.sol";
import {
    MessagingFee,
    MessagingReceipt
} from "@layerzerolabs/lz-evm-protocol-v2/contracts/interfaces/ILayerZeroEndpointV2.sol";

/// @title SendOutbeToSource
/// @notice Send USDT0 from Outbe back to source chain (BSC)
contract SendOutbeToSource is Script {
    error InsufficientTokenBalance(address signer, uint256 balance, uint256 required);
    error InsufficientNativeBalance(address signer, uint256 balance, uint256 required);

    function _getPrivateKey() internal view returns (uint256) {
        string memory key = vm.envString("PRIVATE_KEY");
        return vm.parseUint(key);
    }

    function _toBytes32(address addr) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(addr)));
    }

    /// @notice Send USDT0 from Outbe back to source chain
    function run() external returns (bytes32 guid, uint64 nonce, uint256 nativeFee) {
        uint256 pk = _getPrivateKey();
        address signer = vm.addr(pk);
        uint32 srcEid = uint32(vm.envUint("BSC_EID"));
        address deployer = vm.envAddress("DEPLOYER_ADDRESS");
        address recipient = vm.envAddress("RECIPIENT");
        address oftToken = vm.envAddress("USDT0_OFT_TOKEN");
        uint256 amount = vm.envUint("SEND_AMOUNT_LD");
        uint256 minAmount = vm.envOr("SEND_MIN_AMOUNT_LD", amount);

        SendParam memory sp = SendParam({
            dstEid: srcEid,
            to: _toBytes32(recipient),
            amountLD: amount,
            minAmountLD: minAmount,
            extraOptions: bytes(""),
            composeMsg: bytes(""),
            oftCmd: bytes("")
        });

        MessagingFee memory fee = USDT0OFT(oftToken).quoteSend(sp, false);

        require(USDT0OFT(oftToken).balanceOf(signer) >= amount, "insufficient token balance");
        require(signer.balance >= fee.nativeFee, "insufficient native balance");

        vm.startBroadcast(pk);
        (MessagingReceipt memory receipt, OFTReceipt memory oftReceipt) = USDT0OFT(oftToken).send{value: fee.nativeFee}(
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
