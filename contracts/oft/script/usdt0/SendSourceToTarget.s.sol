// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {USDT} from "../../src/USDT.sol";
import {OFTAdapter} from "../../src/OFTAdapter.sol";

import {SendParam, OFTReceipt} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oft/interfaces/IOFT.sol";
import {
    MessagingFee,
    MessagingReceipt
} from "@layerzerolabs/lz-evm-protocol-v2/contracts/interfaces/ILayerZeroEndpointV2.sol";

/// @title SendSourceToOutbe
/// @notice Send USDT from source chain (BSC) to Outbe
contract SendSourceToOutbe is Script {
    error InsufficientTokenBalance(address signer, uint256 balance, uint256 required);
    error InsufficientNativeBalance(address signer, uint256 balance, uint256 required);
    error AdapterTokenMismatch(address configuredToken, address adapterToken);

    function _getPrivateKey() internal view returns (uint256) {
        string memory key = vm.envString("USDT_PRIVATE_KEY");
        return vm.parseUint(key);
    }

    function _toBytes32(address addr) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(addr)));
    }

    /// @notice Send USDT from source chain (BSC) to Outbe
    function run() external returns (bytes32 guid, uint64 nonce, uint256 nativeFee) {
        uint256 pk = _getPrivateKey();
        address signer = vm.addr(pk);
        uint32 outbeEid = uint32(vm.envUint("OUTBE_EID"));
        address deployer = vm.envAddress("USDT_DEPLOYER_ADDRESS");
        address recipient = vm.envAddress("RECIPIENT");
        address configuredToken = vm.envAddress("USDT_TOKEN");
        address adapter = vm.envAddress("BSC_OFT_ADAPTER");
        address sourceToken = OFTAdapter(adapter).token();
        uint256 amount = vm.envUint("SEND_AMOUNT_LD");
        uint256 minAmount = vm.envOr("SEND_MIN_AMOUNT_LD", amount);

        if (configuredToken != sourceToken) revert AdapterTokenMismatch(configuredToken, sourceToken);

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

        require(USDT(sourceToken).balanceOf(signer) >= amount, "insufficient token balance");
        require(signer.balance >= fee.nativeFee, "insufficient native balance");

        vm.startBroadcast(pk);
        USDT(sourceToken).approve(adapter, amount);
        (MessagingReceipt memory receipt, OFTReceipt memory oftReceipt) = OFTAdapter(adapter)
        .send{value: fee.nativeFee}(
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
