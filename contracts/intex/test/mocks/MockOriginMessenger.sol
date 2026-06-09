// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {MessagingFee, MessagingReceipt} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {IOriginMessenger} from "@contracts/outbe/interfaces/IOriginMessenger.sol";

/// @title MockOriginMessenger
/// @notice Minimal IOriginMessenger that returns canned fees and receipts for unit tests.
/// @dev `nextGuid` is bumped each send so tests can assert per-call event correlation.
contract MockOriginMessenger is IOriginMessenger {
    error MockBridgeRevert();

    bytes32 public nextGuid = keccak256("guid-0");
    uint256 public nativeFee = 0.01 ether;
    bool public sendShouldRevert;

    // Reentrancy attack config: when armed, every send() calls back into telosis.
    address public reentryTarget;
    bytes public reentryCalldata;
    bool public reentryArmed;

    function setSendShouldRevert(bool v) external {
        sendShouldRevert = v;
    }

    function armReentry(address target, bytes calldata data) external {
        reentryTarget = target;
        reentryCalldata = data;
        reentryArmed = true;
    }

    function _consumeGuid() internal returns (bytes32 guid) {
        guid = nextGuid;
        nextGuid = keccak256(abi.encodePacked(nextGuid, "++"));
    }

    function _quote() internal view returns (MessagingFee memory) {
        return MessagingFee({nativeFee: nativeFee, lzTokenFee: 0});
    }

    function _receipt() internal returns (MessagingReceipt memory) {
        if (sendShouldRevert) revert MockBridgeRevert();
        if (reentryArmed) {
            reentryArmed = false;
            (bool ok, bytes memory ret) = reentryTarget.call(reentryCalldata);
            if (!ok) {
                assembly {
                    revert(add(ret, 0x20), mload(ret))
                }
            }
        }
        return MessagingReceipt({guid: _consumeGuid(), nonce: 1, fee: _quote()});
    }

    // --- IOriginMessenger admin ---
    function wire(address, address) external override {}

    // --- Quote/Send pairs ---
    function quoteSendAuctionStageStart(AuctionStageStartParams calldata, bytes calldata, bool)
        external
        view
        override
        returns (MessagingFee memory)
    {
        return _quote();
    }

    function sendAuctionStageStart(AuctionStageStartParams calldata, bytes calldata, MessagingFee calldata, address)
        external
        payable
        override
        returns (MessagingReceipt memory)
    {
        return _receipt();
    }

    function quoteSendAuctionStageReveal(uint32, bool, bytes calldata, bool)
        external
        view
        override
        returns (MessagingFee memory)
    {
        return _quote();
    }

    function sendAuctionStageReveal(uint32, bool, bytes calldata, MessagingFee calldata, address)
        external
        payable
        override
        returns (MessagingReceipt memory)
    {
        return _receipt();
    }

    function quoteSendAuctionStageClearing(uint32, bytes calldata, bool)
        external
        view
        override
        returns (MessagingFee memory)
    {
        return _quote();
    }

    function sendAuctionStageClearing(uint32, bytes calldata, MessagingFee calldata, address)
        external
        payable
        override
        returns (MessagingReceipt memory)
    {
        return _receipt();
    }

    function quoteSendAuctionResult(uint32, uint32, uint64, uint32, bytes calldata, bool)
        external
        view
        override
        returns (MessagingFee memory)
    {
        return _quote();
    }

    function sendAuctionResult(uint32, uint32, uint64, uint32, bytes calldata, MessagingFee calldata, address)
        external
        payable
        override
        returns (MessagingReceipt memory)
    {
        return _receipt();
    }

    function quoteSendIssuanceInstructions(IssuanceInstructionsParams calldata, bytes calldata, bool)
        external
        view
        override
        returns (MessagingFee memory)
    {
        return _quote();
    }

    function sendIssuanceInstructions(
        IssuanceInstructionsParams calldata,
        bytes calldata,
        MessagingFee calldata,
        address
    ) external payable override returns (MessagingReceipt memory) {
        return _receipt();
    }

    function quoteSendRefundInstructions(
        uint32,
        address[] calldata,
        uint64[] calldata,
        uint64[] calldata,
        bytes calldata,
        bool
    ) external view override returns (MessagingFee memory) {
        return _quote();
    }

    function sendRefundInstructions(
        uint32,
        address[] calldata,
        uint64[] calldata,
        uint64[] calldata,
        bytes calldata,
        MessagingFee calldata,
        address
    ) external payable virtual override returns (MessagingReceipt memory) {
        return _receipt();
    }

    function quoteSendMarkCalled(uint32, bytes calldata, bool) external view override returns (MessagingFee memory) {
        return _quote();
    }

    function sendMarkCalled(uint32, bytes calldata, MessagingFee calldata, address)
        external
        payable
        override
        returns (MessagingReceipt memory)
    {
        return _receipt();
    }

    function quoteSendMarkQualified(uint32, bytes calldata, bool) external view override returns (MessagingFee memory) {
        return _quote();
    }

    function sendMarkQualified(uint32, bytes calldata, MessagingFee calldata, address)
        external
        payable
        override
        returns (MessagingReceipt memory)
    {
        return _receipt();
    }

    function sweepNative(address payable, uint256) external override {}

    receive() external payable {}
}
