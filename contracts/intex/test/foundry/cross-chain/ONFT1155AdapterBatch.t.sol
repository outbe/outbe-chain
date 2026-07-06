// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {
    IONFT1155AdapterBatch,
    BatchSendParam,
    MultiRecipientSendParam
} from "@contracts/shared/interfaces/IONFT1155AdapterBatch.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";

/// @title ONFT1155AdapterBatchTest
/// @notice direct coverage for the ONFT-Batch outbound entry points
///         (`batchSend`, `multiSend`, `systemMultiSend`) and their `quote*` views. The inbound
///         `receiveMessage` validation matrix (malformed / duplicate / version / srcChainId) is covered by
///         the sibling cross-chain suites; this file exercises the send-side surface that had no
///         direct test: happy-path delivery, every revert branch, the role gate, and quoting.
contract ONFT1155AdapterBatchTest is CrossChainTest {
    uint32 internal constant SRC_CHAIN_ID = 1;
    uint32 internal constant DST_CHAIN_ID = 2;

    uint256 internal constant FEE = 0.001 ether;

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

    function setUp() public {
        _setUpBridge();
        bridge.setFee(FEE);

        srcToken = DeployProxy.intexNFT1155(admin, admin);
        dstToken = DeployProxy.intexNFT1155(admin, admin);
        srcBatch = DeployProxy.onftAdapterBatch(address(srcToken), address(bridge), admin);
        dstBatch = DeployProxy.onftAdapterBatch(address(dstToken), address(bridge), admin);

        srcBatch.setRemoteMessenger(DST_CHAIN_ID, _interop(DST_CHAIN_ID, address(dstBatch)));
        dstBatch.setRemoteMessenger(SRC_CHAIN_ID, _interop(SRC_CHAIN_ID, address(srcBatch)));

        for (uint32 i = 0; i < 2; i++) {
            uint32 series = i == 0 ? SERIES_A : SERIES_B;
            srcToken.createSeries(CreateSeriesLib.params(series, 1_000_000, 0));
            dstToken.createSeries(CreateSeriesLib.params(series, 1_000_000, 0));
            srcToken.markQualified(series);
            dstToken.markQualified(series);
        }

        srcToken.grantRole(srcToken.RELAYER_ROLE(), address(srcBatch));
        dstToken.grantRole(dstToken.RELAYER_ROLE(), address(dstBatch));
        srcBatch.grantRole(srcBatch.SYSTEM_RELAYER_ROLE(), admin);

        // systemMultiSend draws the bridge fee from the adapter's own float; pre-fund it.
        vm.deal(address(srcBatch), 100 ether);
        vm.deal(sender, 100 ether); // batchSend/multiSend are caller-funded

        // Stock the sender with units on both series so the per-item `crosschainBurn` succeeds.
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

    /// @dev Deliver the packet the src adapter just handed the bridge to the dst adapter.
    function _deliverLast() internal {
        _deliver(SRC_CHAIN_ID, address(srcBatch), address(dstBatch), bridge.lastPayload());
    }

    // ---------------------------------------------------------------
    // constructor — zero-address guards on immutable wiring
    // ---------------------------------------------------------------

    /// @notice `token` is immutable; a zero address permanently bricks every crosschainMint/crosschainBurn path.
    /// @dev Property of the implementation constructor.
    function test_Constructor_RevertsZeroToken() public {
        vm.expectRevert(abi.encodeWithSelector(IONFT1155AdapterBatch.ZeroAddress.selector, "token"));
        new ONFT1155AdapterBatch(address(0), address(bridge));
    }

    /// @notice A zero bridge address is caught by the `ERC7786MessengerBase` constructor guard.
    function test_Constructor_RevertsZeroBridge() public {
        vm.expectRevert(abi.encodeWithSignature("InvalidBridge()"));
        new ONFT1155AdapterBatch(address(srcToken), address(0));
    }

    /// @notice The explicit `ZeroAddress("delegate")` guard in `initialize` rejects a zero
    ///         delegate/owner during proxy initialization.
    function test_Initialize_RevertsZeroDelegate() public {
        ONFT1155AdapterBatch impl = new ONFT1155AdapterBatch(address(srcToken), address(bridge));
        vm.expectRevert(abi.encodeWithSignature("ZeroAddress(string)", "delegate"));
        new ERC1967Proxy(address(impl), abi.encodeCall(ONFT1155AdapterBatch.initialize, (address(0))));
    }

    // ---------------------------------------------------------------
    // batchSend / quoteBatchSend — single recipient, many tokenIds
    // ---------------------------------------------------------------

    function test_BatchSend_HappyPath_CrosschainBurnsSenderAndCrosschainMintsRecipient() public {
        BatchSendParam memory p = BatchSendParam({
            dstChainId: DST_CHAIN_ID,
            to: bytes32(uint256(uint160(recipientA))),
            tokenIds: _u256(TID_A, TID_B),
            amounts: _u256(5, 7)
        });

        uint256 fee = srcBatch.quoteBatchSend(p);

        vm.prank(sender);
        srcBatch.batchSend{value: fee}(p);

        // Sender crosschainBurned on the source for both items.
        assertEq(srcToken.balanceOf(sender, TID_A), 95, "src A crosschainBurned");
        assertEq(srcToken.balanceOf(sender, TID_B), 93, "src B crosschainBurned");

        // Deliver the queued packet; recipient crosschainMinted on the destination.
        _deliverLast();
        assertEq(dstToken.balanceOf(recipientA, TID_A), 5, "dst A crosschainMinted");
        assertEq(dstToken.balanceOf(recipientA, TID_B), 7, "dst B crosschainMinted");
    }

    function test_BatchSend_RevertsEmptyBatch() public {
        BatchSendParam memory p = BatchSendParam({
            dstChainId: DST_CHAIN_ID,
            to: bytes32(uint256(uint160(recipientA))),
            tokenIds: new uint256[](0),
            amounts: new uint256[](0)
        });
        vm.expectRevert(IONFT1155AdapterBatch.EmptyBatch.selector);
        vm.prank(sender);
        srcBatch.batchSend{value: FEE}(p);
    }

    function test_BatchSend_RevertsArrayLengthMismatch() public {
        BatchSendParam memory p = BatchSendParam({
            dstChainId: DST_CHAIN_ID,
            to: bytes32(uint256(uint160(recipientA))),
            tokenIds: _u256(TID_A, TID_B),
            amounts: _u256One(5)
        });
        vm.expectRevert(IONFT1155AdapterBatch.ArrayLengthMismatch.selector);
        vm.prank(sender);
        srcBatch.batchSend{value: FEE}(p);
    }

    function test_BatchSend_RevertsInvalidReceiver_ZeroTo() public {
        // Sender holds balance, so the crosschainBurn loop succeeds; the zero `to` then trips InvalidReceiver
        // inside `_buildBatchMsg`. The whole tx reverts, so the crosschainBurn rolls back too.
        BatchSendParam memory p =
            BatchSendParam({dstChainId: DST_CHAIN_ID, to: bytes32(0), tokenIds: _u256One(TID_A), amounts: _u256One(1)});
        vm.expectRevert(IONFT1155AdapterBatch.InvalidReceiver.selector);
        vm.prank(sender);
        srcBatch.batchSend{value: FEE}(p);

        assertEq(srcToken.balanceOf(sender, TID_A), 100, "crosschainBurn rolled back on revert");
    }

    function test_BatchSend_ZeroAmount_IsNoOpAndDelivers() public {
        // No ZeroValue guard on amounts: a zero-amount item is a burn/crosschainMint of 0 (a no-op) and the
        // send still succeeds. Documents the intended permissive behaviour.
        BatchSendParam memory p = BatchSendParam({
            dstChainId: DST_CHAIN_ID,
            to: bytes32(uint256(uint160(recipientA))),
            tokenIds: _u256One(TID_A),
            amounts: _u256One(0)
        });
        uint256 fee = srcBatch.quoteBatchSend(p);

        vm.prank(sender);
        srcBatch.batchSend{value: fee}(p);

        _deliverLast();
        assertEq(srcToken.balanceOf(sender, TID_A), 100, "sender unchanged for zero-amount item");
        assertEq(dstToken.balanceOf(recipientA, TID_A), 0, "recipient crosschainMinted zero");
    }

    function test_QuoteBatchSend_ReturnsNonZeroNativeFee() public view {
        BatchSendParam memory p = BatchSendParam({
            dstChainId: DST_CHAIN_ID,
            to: bytes32(uint256(uint160(recipientA))),
            tokenIds: _u256(TID_A, TID_B),
            amounts: _u256(5, 7)
        });
        uint256 fee = srcBatch.quoteBatchSend(p);
        assertEq(fee, FEE, "native fee quoted");
    }

    // --- Caller-funded fee accounting ---

    function test_BatchSend_RevertsBelowFee() public {
        // batchSend is caller-funded: less than the quoted fee reverts (it must not draw the float).
        BatchSendParam memory p = BatchSendParam({
            dstChainId: DST_CHAIN_ID,
            to: bytes32(uint256(uint160(recipientA))),
            tokenIds: _u256One(TID_A),
            amounts: _u256One(1)
        });
        uint256 fee = srcBatch.quoteBatchSend(p);
        vm.expectRevert(abi.encodeWithSignature("MsgValueBelowFee(uint256,uint256)", fee - 1, fee));
        vm.prank(sender);
        srcBatch.batchSend{value: fee - 1}(p);
    }

    function test_BatchSend_RefundsExcessValue() public {
        // Excess native value above the fee is refunded to the caller, not absorbed into the float.
        BatchSendParam memory p = BatchSendParam({
            dstChainId: DST_CHAIN_ID,
            to: bytes32(uint256(uint160(recipientA))),
            tokenIds: _u256One(TID_A),
            amounts: _u256One(1)
        });
        uint256 fee = srcBatch.quoteBatchSend(p);
        uint256 floatBefore = address(srcBatch).balance;
        uint256 senderBefore = sender.balance;

        vm.prank(sender);
        srcBatch.batchSend{value: fee + 1 ether}(p);

        // Caller paid only the fee; the 1 ether excess came back; the float was untouched.
        assertEq(sender.balance, senderBefore - fee, "only the fee charged");
        assertEq(address(srcBatch).balance, floatBefore, "system float untouched");
    }

    // ---------------------------------------------------------------
    // multiSend / quoteMultiSend — many recipients
    // ---------------------------------------------------------------

    function test_MultiSend_HappyPath_CrosschainMintsEachRecipient() public {
        bytes32[] memory recipients = new bytes32[](2);
        recipients[0] = bytes32(uint256(uint160(recipientA)));
        recipients[1] = bytes32(uint256(uint160(recipientB)));

        MultiRecipientSendParam memory p = MultiRecipientSendParam({
            dstChainId: DST_CHAIN_ID, recipients: recipients, tokenIds: _u256(TID_A, TID_B), amounts: _u256(3, 4)
        });

        uint256 fee = srcBatch.quoteMultiSend(p);

        vm.prank(sender);
        srcBatch.multiSend{value: fee}(p);

        assertEq(srcToken.balanceOf(sender, TID_A), 97, "src A crosschainBurned");
        assertEq(srcToken.balanceOf(sender, TID_B), 96, "src B crosschainBurned");

        _deliverLast();
        assertEq(dstToken.balanceOf(recipientA, TID_A), 3, "recipientA crosschainMinted A");
        assertEq(dstToken.balanceOf(recipientB, TID_B), 4, "recipientB crosschainMinted B");
    }

    function test_MultiSend_RevertsEmptyBatch() public {
        MultiRecipientSendParam memory p = MultiRecipientSendParam({
            dstChainId: DST_CHAIN_ID,
            recipients: new bytes32[](0),
            tokenIds: new uint256[](0),
            amounts: new uint256[](0)
        });
        vm.expectRevert(IONFT1155AdapterBatch.EmptyBatch.selector);
        vm.prank(sender);
        srcBatch.multiSend{value: FEE}(p);
    }

    function test_MultiSend_RevertsArrayLengthMismatch() public {
        bytes32[] memory recipients = new bytes32[](2);
        recipients[0] = bytes32(uint256(uint160(recipientA)));
        recipients[1] = bytes32(uint256(uint160(recipientB)));

        MultiRecipientSendParam memory p = MultiRecipientSendParam({
            dstChainId: DST_CHAIN_ID,
            recipients: recipients,
            tokenIds: _u256One(TID_A), // length 1 vs 2 recipients
            amounts: _u256(3, 4)
        });
        vm.expectRevert(IONFT1155AdapterBatch.ArrayLengthMismatch.selector);
        vm.prank(sender);
        srcBatch.multiSend{value: FEE}(p);
    }

    function test_MultiSend_RevertsInvalidReceiver_ZeroRecipient() public {
        bytes32[] memory recipients = new bytes32[](1);
        recipients[0] = bytes32(0);

        MultiRecipientSendParam memory p = MultiRecipientSendParam({
            dstChainId: DST_CHAIN_ID, recipients: recipients, tokenIds: _u256One(TID_A), amounts: _u256One(1)
        });
        vm.expectRevert(IONFT1155AdapterBatch.InvalidReceiver.selector);
        vm.prank(sender);
        srcBatch.multiSend{value: FEE}(p);
    }

    function test_QuoteMultiSend_ReturnsNonZeroNativeFee() public view {
        bytes32[] memory recipients = new bytes32[](1);
        recipients[0] = bytes32(uint256(uint160(recipientA)));
        MultiRecipientSendParam memory p = MultiRecipientSendParam({
            dstChainId: DST_CHAIN_ID, recipients: recipients, tokenIds: _u256One(TID_A), amounts: _u256One(1)
        });
        uint256 fee = srcBatch.quoteMultiSend(p);
        assertEq(fee, FEE, "native fee quoted");
    }

    // ---------------------------------------------------------------
    // systemMultiSend / quoteSystemMultiSend — role-gated migration
    // ---------------------------------------------------------------

    function test_SystemMultiSend_HappyPath_CrosschainMintsHolders() public {
        address[] memory holders = new address[](1);
        holders[0] = sender;
        uint256[] memory amounts = _u256One(5);

        // Relay-float funded: NOT payable, fee drawn from the pre-funded adapter balance.
        srcBatch.systemMultiSend(TID_A, holders, amounts, DST_CHAIN_ID);

        assertEq(srcToken.balanceOf(sender, TID_A), 95, "holder crosschainBurned on source");

        _deliverLast();
        assertEq(dstToken.balanceOf(sender, TID_A), 5, "holder crosschainMinted on destination");
    }

    function test_SystemMultiSend_RevertsWhenNotSystemRelayer() public {
        address[] memory holders = new address[](1);
        holders[0] = sender;
        uint256[] memory amounts = _u256One(1);

        vm.expectRevert(
            abi.encodeWithSelector(
                IAccessControl.AccessControlUnauthorizedAccount.selector, sender, srcBatch.SYSTEM_RELAYER_ROLE()
            )
        );
        vm.prank(sender);
        srcBatch.systemMultiSend(TID_A, holders, amounts, DST_CHAIN_ID);
    }

    function test_SystemMultiSend_RevertsEmptyBatch() public {
        address[] memory holders = new address[](0);
        uint256[] memory amounts = new uint256[](0);
        vm.expectRevert(IONFT1155AdapterBatch.EmptyBatch.selector);
        srcBatch.systemMultiSend(TID_A, holders, amounts, DST_CHAIN_ID);
    }

    function test_SystemMultiSend_RevertsArrayLengthMismatch() public {
        address[] memory holders = new address[](1);
        holders[0] = sender;
        uint256[] memory amounts = _u256(1, 2); // length 2 vs 1 holder
        vm.expectRevert(IONFT1155AdapterBatch.ArrayLengthMismatch.selector);
        srcBatch.systemMultiSend(TID_A, holders, amounts, DST_CHAIN_ID);
    }

    function test_QuoteSystemMultiSend_ReturnsNonZeroNativeFee() public view {
        address[] memory holders = new address[](1);
        holders[0] = sender;
        uint256[] memory amounts = _u256One(1);
        uint256 fee = srcBatch.quoteSystemMultiSend(TID_A, holders, amounts, DST_CHAIN_ID);
        assertEq(fee, FEE, "native fee quoted");
    }
}
