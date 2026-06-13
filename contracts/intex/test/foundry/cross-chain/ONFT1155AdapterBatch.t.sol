// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {MessagingFee} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";

import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {
    IONFT1155AdapterBatch,
    BatchSendParam,
    MultiRecipientSendParam
} from "@contracts/shared/interfaces/IONFT1155AdapterBatch.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";

/// @title ONFT1155AdapterBatchTest
/// @notice direct coverage for the ONFT-Batch outbound entry points
///         (`batchSend`, `multiSend`, `systemMultiSend`) and their `quote*` views. The inbound
///         `_lzReceive` validation matrix (malformed / duplicate / version / srcEid) is covered by
///         the sibling cross-chain suites; this file exercises the send-side surface that had no
///         direct test: happy-path delivery, every revert branch, the role gate, and quoting.
contract ONFT1155AdapterBatchTest is TestHelperOz5 {
    uint32 internal constant SRC_EID = 1;
    uint32 internal constant DST_EID = 2;

    ONFT1155AdapterBatch internal srcBatch;
    ONFT1155AdapterBatch internal dstBatch;
    IntexNFT1155 internal srcToken;
    IntexNFT1155 internal dstToken;

    address internal admin = address(this);
    address internal sender = address(0xB0B);
    address internal recipientA = address(0xA11CE);
    address internal recipientB = address(0xCAFE);

    uint32 internal constant SERIES_A = 20260601;
    uint32 internal constant SERIES_B = 20260602;
    uint256 internal constant TID_A = uint256(SERIES_A);
    uint256 internal constant TID_B = uint256(SERIES_B);

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        srcToken = DeployProxy.intexNFT1155(admin, admin);
        dstToken = DeployProxy.intexNFT1155(admin, admin);
        srcBatch = new ONFT1155AdapterBatch(address(srcToken), address(endpoints[SRC_EID]), admin);
        dstBatch = new ONFT1155AdapterBatch(address(dstToken), address(endpoints[DST_EID]), admin);

        address[] memory oapps = new address[](2);
        oapps[0] = address(srcBatch);
        oapps[1] = address(dstBatch);
        this.wireOApps(oapps);

        for (uint32 i = 0; i < 2; i++) {
            uint32 series = i == 0 ? SERIES_A : SERIES_B;
            srcToken.createSeries(series, 1_000_000, 0);
            dstToken.createSeries(series, 1_000_000, 0);
            srcToken.markQualified(series);
            dstToken.markQualified(series);
        }

        srcToken.grantRole(srcToken.RELAYER_ROLE(), address(srcBatch));
        dstToken.grantRole(dstToken.RELAYER_ROLE(), address(dstBatch));
        srcBatch.grantRole(srcBatch.SYSTEM_RELAYER_ROLE(), admin);

        // Both send paths draw the LZ fee from the adapter's own balance (`_payNative` override),
        // so the adapter is pre-funded rather than the caller forwarding msg.value.
        vm.deal(address(srcBatch), 100 ether);
        vm.deal(sender, 100 ether); // batchSend/multiSend are caller-funded

        // Stock the sender with units on both series so the per-item `debit` succeeds.
        srcToken.mint(sender, 100, SERIES_A);
        srcToken.mint(sender, 100, SERIES_B);
    }

    // --- helpers ---

    function _u256(uint256 a, uint256 b) internal pure returns (uint256[] memory arr) {
        arr = new uint256[](2);
        arr[0] = a;
        arr[1] = b;
    }

    function _u256One(uint256 a) internal pure returns (uint256[] memory arr) {
        arr = new uint256[](1);
        arr[0] = a;
    }

    // ---------------------------------------------------------------
    // constructor — zero-address guards on immutable wiring
    // ---------------------------------------------------------------

    /// @notice `token` is immutable; a zero address permanently bricks every credit/debit path.
    function test_Constructor_RevertsZeroToken() public {
        vm.expectRevert(abi.encodeWithSelector(IONFT1155AdapterBatch.ZeroAddress.selector, "token"));
        new ONFT1155AdapterBatch(address(0), address(endpoints[SRC_EID]), admin);
    }

    /// @notice The OApp base only reverts opaquely on a zero `_lzEndpoint` (the `setDelegate` call
    ///         hits empty code). The guard surfaces the typed `ZeroAddress("lzEndpoint")` instead.
    function test_Constructor_RevertsZeroLzEndpoint() public {
        vm.expectRevert(abi.encodeWithSelector(IONFT1155AdapterBatch.ZeroAddress.selector, "lzEndpoint"));
        new ONFT1155AdapterBatch(address(srcToken), address(0), admin);
    }

    /// @notice `_delegate` (also the owner) is rejected by the OZ `Ownable` base constructor, which
    ///         linearizes ahead of `OAppCore`. Documents that the framework — not an added guard —
    ///         closes the zero-delegate brick path.
    function test_Constructor_RevertsZeroDelegate_ViaFramework() public {
        vm.expectRevert(abi.encodeWithSignature("OwnableInvalidOwner(address)", address(0)));
        new ONFT1155AdapterBatch(address(srcToken), address(endpoints[SRC_EID]), address(0));
    }

    // ---------------------------------------------------------------
    // batchSend / quoteBatchSend — single recipient, many tokenIds
    // ---------------------------------------------------------------

    function test_BatchSend_HappyPath_DebitsSenderAndCreditsRecipient() public {
        BatchSendParam memory p = BatchSendParam({
            dstEid: DST_EID,
            to: addressToBytes32(recipientA),
            tokenIds: _u256(TID_A, TID_B),
            amounts: _u256(5, 7),
            extraOptions: ""
        });

        MessagingFee memory fee = srcBatch.quoteBatchSend(p, false);

        vm.prank(sender);
        srcBatch.batchSend{value: fee.nativeFee}(p, fee, sender);

        // Sender debited on the source for both items.
        assertEq(srcToken.balanceOf(sender, TID_A), 95, "src A debited");
        assertEq(srcToken.balanceOf(sender, TID_B), 93, "src B debited");

        // Deliver the queued packet; recipient credited on the destination.
        verifyPackets(DST_EID, addressToBytes32(address(dstBatch)));
        assertEq(dstToken.balanceOf(recipientA, TID_A), 5, "dst A credited");
        assertEq(dstToken.balanceOf(recipientA, TID_B), 7, "dst B credited");
    }

    function test_BatchSend_RevertsEmptyBatch() public {
        BatchSendParam memory p = BatchSendParam({
            dstEid: DST_EID,
            to: addressToBytes32(recipientA),
            tokenIds: new uint256[](0),
            amounts: new uint256[](0),
            extraOptions: ""
        });
        MessagingFee memory fee = MessagingFee(1 ether, 0);
        vm.expectRevert(IONFT1155AdapterBatch.EmptyBatch.selector);
        vm.prank(sender);
        srcBatch.batchSend{value: fee.nativeFee}(p, fee, sender);
    }

    function test_BatchSend_RevertsArrayLengthMismatch() public {
        BatchSendParam memory p = BatchSendParam({
            dstEid: DST_EID,
            to: addressToBytes32(recipientA),
            tokenIds: _u256(TID_A, TID_B),
            amounts: _u256One(5),
            extraOptions: ""
        });
        MessagingFee memory fee = MessagingFee(1 ether, 0);
        vm.expectRevert(IONFT1155AdapterBatch.ArrayLengthMismatch.selector);
        vm.prank(sender);
        srcBatch.batchSend{value: fee.nativeFee}(p, fee, sender);
    }

    function test_BatchSend_RevertsInvalidReceiver_ZeroTo() public {
        // Sender holds balance, so the debit loop succeeds; the zero `to` then trips InvalidReceiver
        // inside `_buildBatchMsgAndOptions`. The whole tx reverts, so the debit rolls back too.
        BatchSendParam memory p = BatchSendParam({
            dstEid: DST_EID, to: bytes32(0), tokenIds: _u256One(TID_A), amounts: _u256One(1), extraOptions: ""
        });
        MessagingFee memory fee = MessagingFee(1 ether, 0);
        vm.expectRevert(IONFT1155AdapterBatch.InvalidReceiver.selector);
        vm.prank(sender);
        srcBatch.batchSend{value: fee.nativeFee}(p, fee, sender);

        assertEq(srcToken.balanceOf(sender, TID_A), 100, "debit rolled back on revert");
    }

    function test_BatchSend_ZeroAmount_IsNoOpAndDelivers() public {
        // No ZeroValue guard on amounts: a zero-amount item is a burn/credit of 0 (a no-op) and the
        // send still succeeds. Documents the intended permissive behaviour.
        BatchSendParam memory p = BatchSendParam({
            dstEid: DST_EID,
            to: addressToBytes32(recipientA),
            tokenIds: _u256One(TID_A),
            amounts: _u256One(0),
            extraOptions: ""
        });
        MessagingFee memory fee = srcBatch.quoteBatchSend(p, false);

        vm.prank(sender);
        srcBatch.batchSend{value: fee.nativeFee}(p, fee, sender);

        verifyPackets(DST_EID, addressToBytes32(address(dstBatch)));
        assertEq(srcToken.balanceOf(sender, TID_A), 100, "sender unchanged for zero-amount item");
        assertEq(dstToken.balanceOf(recipientA, TID_A), 0, "recipient credited zero");
    }

    function test_QuoteBatchSend_ReturnsNonZeroNativeFee() public view {
        BatchSendParam memory p = BatchSendParam({
            dstEid: DST_EID,
            to: addressToBytes32(recipientA),
            tokenIds: _u256(TID_A, TID_B),
            amounts: _u256(5, 7),
            extraOptions: ""
        });
        MessagingFee memory fee = srcBatch.quoteBatchSend(p, false);
        assertGt(fee.nativeFee, 0, "native fee quoted");
        assertEq(fee.lzTokenFee, 0, "no lz-token fee requested");
    }

    // --- Caller-funded fee accounting ---

    function test_BatchSend_RevertsBelowFee() public {
        // batchSend is caller-funded: less than the quoted fee reverts (it must not draw the float).
        BatchSendParam memory p = BatchSendParam({
            dstEid: DST_EID,
            to: addressToBytes32(recipientA),
            tokenIds: _u256One(TID_A),
            amounts: _u256One(1),
            extraOptions: ""
        });
        MessagingFee memory fee = srcBatch.quoteBatchSend(p, false);
        vm.expectRevert(
            abi.encodeWithSelector(IONFT1155AdapterBatch.MsgValueBelowFee.selector, fee.nativeFee - 1, fee.nativeFee)
        );
        vm.prank(sender);
        srcBatch.batchSend{value: fee.nativeFee - 1}(p, fee, sender);
    }

    function test_BatchSend_RefundsExcessValue() public {
        // Excess native value above the fee is refunded to the caller, not absorbed into the float.
        BatchSendParam memory p = BatchSendParam({
            dstEid: DST_EID,
            to: addressToBytes32(recipientA),
            tokenIds: _u256One(TID_A),
            amounts: _u256One(1),
            extraOptions: ""
        });
        MessagingFee memory fee = srcBatch.quoteBatchSend(p, false);
        uint256 floatBefore = address(srcBatch).balance;
        uint256 senderBefore = sender.balance;

        vm.prank(sender);
        srcBatch.batchSend{value: fee.nativeFee + 1 ether}(p, fee, sender);

        // Caller paid only the fee; the 1 ether excess came back; the float was untouched.
        assertEq(sender.balance, senderBefore - fee.nativeFee, "only the fee charged");
        assertEq(address(srcBatch).balance, floatBefore, "system float untouched");
    }

    // ---------------------------------------------------------------
    // multiSend / quoteMultiSend — many recipients
    // ---------------------------------------------------------------

    function test_MultiSend_HappyPath_CreditsEachRecipient() public {
        bytes32[] memory recipients = new bytes32[](2);
        recipients[0] = addressToBytes32(recipientA);
        recipients[1] = addressToBytes32(recipientB);

        MultiRecipientSendParam memory p = MultiRecipientSendParam({
            dstEid: DST_EID,
            recipients: recipients,
            tokenIds: _u256(TID_A, TID_B),
            amounts: _u256(3, 4),
            extraOptions: ""
        });

        MessagingFee memory fee = srcBatch.quoteMultiSend(p, false);

        vm.prank(sender);
        srcBatch.multiSend{value: fee.nativeFee}(p, fee, sender);

        assertEq(srcToken.balanceOf(sender, TID_A), 97, "src A debited");
        assertEq(srcToken.balanceOf(sender, TID_B), 96, "src B debited");

        verifyPackets(DST_EID, addressToBytes32(address(dstBatch)));
        assertEq(dstToken.balanceOf(recipientA, TID_A), 3, "recipientA credited A");
        assertEq(dstToken.balanceOf(recipientB, TID_B), 4, "recipientB credited B");
    }

    function test_MultiSend_RevertsEmptyBatch() public {
        MultiRecipientSendParam memory p = MultiRecipientSendParam({
            dstEid: DST_EID,
            recipients: new bytes32[](0),
            tokenIds: new uint256[](0),
            amounts: new uint256[](0),
            extraOptions: ""
        });
        MessagingFee memory fee = MessagingFee(1 ether, 0);
        vm.expectRevert(IONFT1155AdapterBatch.EmptyBatch.selector);
        vm.prank(sender);
        srcBatch.multiSend{value: fee.nativeFee}(p, fee, sender);
    }

    function test_MultiSend_RevertsArrayLengthMismatch() public {
        bytes32[] memory recipients = new bytes32[](2);
        recipients[0] = addressToBytes32(recipientA);
        recipients[1] = addressToBytes32(recipientB);

        MultiRecipientSendParam memory p = MultiRecipientSendParam({
            dstEid: DST_EID,
            recipients: recipients,
            tokenIds: _u256One(TID_A), // length 1 vs 2 recipients
            amounts: _u256(3, 4),
            extraOptions: ""
        });
        MessagingFee memory fee = MessagingFee(1 ether, 0);
        vm.expectRevert(IONFT1155AdapterBatch.ArrayLengthMismatch.selector);
        vm.prank(sender);
        srcBatch.multiSend{value: fee.nativeFee}(p, fee, sender);
    }

    function test_MultiSend_RevertsInvalidReceiver_ZeroRecipient() public {
        bytes32[] memory recipients = new bytes32[](1);
        recipients[0] = bytes32(0);

        MultiRecipientSendParam memory p = MultiRecipientSendParam({
            dstEid: DST_EID, recipients: recipients, tokenIds: _u256One(TID_A), amounts: _u256One(1), extraOptions: ""
        });
        MessagingFee memory fee = MessagingFee(1 ether, 0);
        vm.expectRevert(IONFT1155AdapterBatch.InvalidReceiver.selector);
        vm.prank(sender);
        srcBatch.multiSend{value: fee.nativeFee}(p, fee, sender);
    }

    function test_QuoteMultiSend_ReturnsNonZeroNativeFee() public view {
        bytes32[] memory recipients = new bytes32[](1);
        recipients[0] = addressToBytes32(recipientA);
        MultiRecipientSendParam memory p = MultiRecipientSendParam({
            dstEid: DST_EID, recipients: recipients, tokenIds: _u256One(TID_A), amounts: _u256One(1), extraOptions: ""
        });
        MessagingFee memory fee = srcBatch.quoteMultiSend(p, false);
        assertGt(fee.nativeFee, 0, "native fee quoted");
    }

    // ---------------------------------------------------------------
    // systemMultiSend / quoteSystemMultiSend — role-gated migration
    // (happy-path E2E is covered in DynamicLzGas; here: gates + quoting)
    // ---------------------------------------------------------------

    function test_SystemMultiSend_RevertsWhenNotSystemRelayer() public {
        address[] memory holders = new address[](1);
        holders[0] = sender;
        uint256[] memory amounts = _u256One(1);

        MessagingFee memory fee = MessagingFee(1 ether, 0);
        vm.expectRevert(
            abi.encodeWithSelector(
                IAccessControl.AccessControlUnauthorizedAccount.selector, sender, srcBatch.SYSTEM_RELAYER_ROLE()
            )
        );
        vm.prank(sender);
        srcBatch.systemMultiSend(TID_A, holders, amounts, DST_EID, "", fee);
    }

    function test_SystemMultiSend_RevertsEmptyBatch() public {
        address[] memory holders = new address[](0);
        uint256[] memory amounts = new uint256[](0);
        MessagingFee memory fee = MessagingFee(1 ether, 0);
        vm.expectRevert(IONFT1155AdapterBatch.EmptyBatch.selector);
        srcBatch.systemMultiSend(TID_A, holders, amounts, DST_EID, "", fee);
    }

    function test_SystemMultiSend_RevertsArrayLengthMismatch() public {
        address[] memory holders = new address[](1);
        holders[0] = sender;
        uint256[] memory amounts = _u256(1, 2); // length 2 vs 1 holder
        MessagingFee memory fee = MessagingFee(1 ether, 0);
        vm.expectRevert(IONFT1155AdapterBatch.ArrayLengthMismatch.selector);
        srcBatch.systemMultiSend(TID_A, holders, amounts, DST_EID, "", fee);
    }

    function test_QuoteSystemMultiSend_ReturnsNonZeroNativeFee() public view {
        address[] memory holders = new address[](1);
        holders[0] = sender;
        uint256[] memory amounts = _u256One(1);
        MessagingFee memory fee = srcBatch.quoteSystemMultiSend(TID_A, holders, amounts, DST_EID, "", false);
        assertGt(fee.nativeFee, 0, "native fee quoted");
    }
}
