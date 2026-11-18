// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IERC7786Recipient} from "@openzeppelin/contracts/interfaces/draft-IERC7786.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {ERC7786TokenBridge} from "../../src/ERC7786TokenBridge.sol";
import {IERC7786TokenReceiver} from "../../src/interfaces/IERC7786TokenReceiver.sol";
import {USDT} from "../../src/canonical/USDT.sol";
import {BridgeableERC20Stable} from "../../src/synthetic/BridgeableERC20Stable.sol";
import {MockERC7786Bridge} from "../mocks/MockERC7786Bridge.sol";
import {MockStablecoin} from "../mocks/MockStablecoin.sol";
import {TokenBridgeProxy} from "../utils/TokenBridgeProxy.sol";

/// @dev Records the hook invocation; configurable to misbehave.
contract MockTokenReceiver is IERC7786TokenReceiver {
    IERC20 public immutable token;

    uint256 public calls;
    uint32 public lastSourceDomain;
    bytes public lastFrom;
    uint256 public lastAmount;
    bytes public lastExtraData;
    uint256 public balanceAtHook;

    bool public returnWrongMagic;
    bool public revertOnHook;

    constructor(IERC20 token_) {
        token = token_;
    }

    function setReturnWrongMagic(bool v) external {
        returnWrongMagic = v;
    }

    function setRevertOnHook(bool v) external {
        revertOnHook = v;
    }

    function onCrosschainTokensReceived(
        uint32 sourceDomain,
        bytes calldata from,
        uint256 amount,
        bytes calldata extraData
    ) external returns (bytes4) {
        if (revertOnHook) revert("hook revert");

        calls++;
        lastSourceDomain = sourceDomain;
        lastFrom = from;
        lastAmount = amount;
        lastExtraData = extraData;
        balanceAtHook = token.balanceOf(address(this));

        if (returnWrongMagic) return 0xdeadbeef;
        return IERC7786TokenReceiver.onCrosschainTokensReceived.selector;
    }
}

contract ERC7786TokenBridgeComposedTest is Test {
    uint32 internal constant BNB = 56;
    uint32 internal constant OUTBE = 12_121;

    address internal alice = makeAddr("alice");
    address internal bob = makeAddr("bob");

    MockERC7786Bridge internal bnbGateway;
    MockERC7786Bridge internal outbeGateway;

    USDT internal usdt;
    BridgeableERC20Stable internal usdt0;
    ERC7786TokenBridge internal bnbUsdtBridge;
    ERC7786TokenBridge internal outbeUsdt0Bridge;

    MockStablecoin internal stable;
    ERC7786TokenBridge internal bnbStableBridge;
    ERC7786TokenBridge internal outbeStableBridge;

    MockTokenReceiver internal receiver;

    function setUp() public {
        bnbGateway = new MockERC7786Bridge(BNB);
        outbeGateway = new MockERC7786Bridge(OUTBE);
        bnbGateway.setRemoteBridge(OUTBE, outbeGateway);
        outbeGateway.setRemoteBridge(BNB, bnbGateway);

        usdt = new USDT();
        ERC7786TokenBridge.Peer[] memory peers = new ERC7786TokenBridge.Peer[](1);
        peers[0] = ERC7786TokenBridge.Peer(OUTBE, _nextAddress(4));
        bnbUsdtBridge = TokenBridgeProxy.deploy(
            address(usdt), address(bnbGateway), ERC7786TokenBridge.TokenBridgeMode.LockUnlock, address(this), peers
        );

        usdt0 = new BridgeableERC20Stable("USDT0", "USDT0", 6, 840, _nextAddress(2));
        peers[0] = ERC7786TokenBridge.Peer(BNB, address(bnbUsdtBridge));
        outbeUsdt0Bridge = TokenBridgeProxy.deploy(
            address(usdt0), address(outbeGateway), ERC7786TokenBridge.TokenBridgeMode.BurnMint, address(this), peers
        );

        receiver = new MockTokenReceiver(IERC20(address(usdt0)));

        stable = new MockStablecoin("USD Tether", "USDT", address(this));
        peers[0] = ERC7786TokenBridge.Peer(OUTBE, _nextAddress(3));
        bnbStableBridge = TokenBridgeProxy.deploy(
            address(usdt), address(bnbGateway), ERC7786TokenBridge.TokenBridgeMode.LockUnlock, address(this), peers
        );
        peers[0] = ERC7786TokenBridge.Peer(BNB, address(bnbStableBridge));
        outbeStableBridge = TokenBridgeProxy.deploy(
            address(stable),
            address(outbeGateway),
            ERC7786TokenBridge.TokenBridgeMode.IssuerBurnMint,
            address(this),
            peers
        );
        stable.grantIssuer(address(outbeStableBridge));
    }

    function test_SendAndCall_DeliversTokensThenHook() public {
        uint256 amount = 100e6;
        bytes memory extraData = abi.encode(uint32(20_260_706));
        usdt.mint(alice, amount);

        vm.startPrank(alice);
        usdt.approve(address(bnbUsdtBridge), amount);
        bnbUsdtBridge.sendAndCall(OUTBE, address(receiver), amount, extraData, 500_000);
        vm.stopPrank();

        assertEq(usdt0.balanceOf(address(receiver)), amount);
        assertEq(receiver.calls(), 1);
        assertEq(receiver.lastSourceDomain(), BNB);
        assertEq(receiver.lastFrom(), _interop(block.chainid, alice));
        assertEq(receiver.lastAmount(), amount);
        assertEq(receiver.lastExtraData(), extraData);
        // Tokens were credited before the hook ran.
        assertEq(receiver.balanceAtHook(), amount);
        assertEq(outbeUsdt0Bridge.failedDeliveries(bytes32(0)).sender, address(0), "no record");
    }

    function test_Send_PlainDoesNotInvokeHook() public {
        uint256 amount = 100e6;
        usdt.mint(alice, amount);

        vm.startPrank(alice);
        usdt.approve(address(bnbUsdtBridge), amount);
        bnbUsdtBridge.send(OUTBE, address(receiver), amount);
        vm.stopPrank();

        assertEq(usdt0.balanceOf(address(receiver)), amount);
        assertEq(receiver.calls(), 0);
    }

    function test_RevertWhen_SendAndCallWithEmptyExtraData() public {
        usdt.mint(alice, 1);
        vm.startPrank(alice);
        usdt.approve(address(bnbUsdtBridge), 1);
        vm.expectRevert(ERC7786TokenBridge.EmptyExtraData.selector);
        bnbUsdtBridge.sendAndCall(OUTBE, address(receiver), 1, "", 0);
        vm.stopPrank();
    }

    function test_RevertWhen_SendAndCallToZeroRecipient() public {
        usdt.mint(alice, 1);

        vm.startPrank(alice);
        usdt.approve(address(bnbUsdtBridge), 1);
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.InvalidRecipient.selector, address(0)));
        bnbUsdtBridge.sendAndCall(OUTBE, address(0), 1, abi.encode(uint32(1)), 0);
        vm.stopPrank();

        assertEq(usdt.balanceOf(alice), 1);
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), 0);
    }

    function test_HookReverts_StoresRecordAndCreditsNothing() public {
        receiver.setRevertOnHook(true);
        _assertFailedDelivery(address(receiver));
    }

    function test_HookReturnsWrongMagic_StoresRecordAndCreditsNothing() public {
        receiver.setReturnWrongMagic(true);
        _assertFailedDelivery(address(receiver));
        // The hook ran, but the bridge rolled back its effects with the credit.
        assertEq(receiver.calls(), 0);
    }

    function test_SendAndCallToEOA_StoresRecordAndCreditsNothing() public {
        _assertFailedDelivery(bob);
    }

    function test_ReceiveMessage_KeysRecordByReceiveId() public {
        receiver.setRevertOnHook(true);
        bytes32 receiveId = keccak256("message");
        bytes memory payload =
            abi.encode(_interop(block.chainid, alice), address(receiver), uint256(7), uint8(6), abi.encode(uint32(1)));

        vm.prank(address(outbeGateway));
        bytes4 magic = outbeUsdt0Bridge.receiveMessage(receiveId, _interop(BNB, address(bnbUsdtBridge)), payload);

        assertEq(magic, IERC7786Recipient.receiveMessage.selector);
        _assertRecord(outbeUsdt0Bridge, receiveId, BNB, alice, address(receiver), 7);
        assertEq(outbeUsdt0Bridge.failedDeliveries(bytes32(0)).sender, address(0));
    }

    function test_ReclaimToSource_UnlocksOnSourceChain() public {
        uint256 amount = 100e6;
        receiver.setRevertOnHook(true);
        _sendAndCall(bnbUsdtBridge, usdt, alice, OUTBE, address(receiver), amount);

        uint256 fee = 1 ether;
        vm.deal(bob, fee);
        vm.expectEmit(address(bnbUsdtBridge));
        emit ERC7786TokenBridge.CrosschainTransferReceived(
            bytes32(0), OUTBE, _interop(block.chainid, address(receiver)), alice, amount
        );
        vm.expectEmit(address(outbeUsdt0Bridge));
        emit ERC7786TokenBridge.CrosschainDeliveryReclaimed(bytes32(0), bytes32(0), BNB, alice, amount);
        vm.prank(bob);
        outbeUsdt0Bridge.reclaimToSource{value: fee}(bytes32(0));

        assertEq(usdt.balanceOf(alice), amount, "sender repaid");
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), 0, "unlocked");
        assertEq(usdt0.totalSupply(), 0, "nothing minted");
        assertEq(address(outbeGateway).balance, fee, "fee forwarded");
        assertEq(outbeUsdt0Bridge.failedDeliveries(bytes32(0)).sender, address(0), "record deleted");
        assertEq(receiver.calls(), 0, "no hook");
    }

    function test_ReclaimToSource_MintsOnBurnMintSource() public {
        uint256 amount = 100e6;
        usdt.mint(alice, amount);
        vm.startPrank(alice);
        usdt.approve(address(bnbUsdtBridge), amount);
        bnbUsdtBridge.send(OUTBE, alice, amount);
        vm.stopPrank();

        MockTokenReceiver bnbReceiver = new MockTokenReceiver(IERC20(address(usdt)));
        bnbReceiver.setRevertOnHook(true);
        vm.prank(alice);
        outbeUsdt0Bridge.sendAndCall(BNB, address(bnbReceiver), amount, abi.encode(uint32(1)), 0);

        assertEq(usdt0.balanceOf(alice), 0, "burned on source");
        assertEq(usdt.balanceOf(address(bnbReceiver)), 0, "nothing unlocked");
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), amount, "still locked");
        _assertRecord(bnbUsdtBridge, bytes32(0), OUTBE, alice, address(bnbReceiver), amount);

        bnbUsdtBridge.reclaimToSource(bytes32(0));

        assertEq(usdt0.balanceOf(alice), amount, "minted back");
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), amount, "still locked");
        assertEq(bnbReceiver.calls(), 0, "no hook");
    }

    function test_ReclaimToSource_MintsOnIssuerSource() public {
        uint256 amount = 100e6;
        stable.grantIssuer(address(this));
        stable.mint(alice, amount);

        MockTokenReceiver bnbReceiver = new MockTokenReceiver(IERC20(address(usdt)));
        bnbReceiver.setRevertOnHook(true);
        vm.startPrank(alice);
        stable.approve(address(outbeStableBridge), amount);
        outbeStableBridge.sendAndCall(BNB, address(bnbReceiver), amount, abi.encode(uint32(1)), 0);
        vm.stopPrank();

        assertEq(stable.balanceOf(alice), 0, "burned on source");
        _assertRecord(bnbStableBridge, bytes32(0), OUTBE, alice, address(bnbReceiver), amount);

        bnbStableBridge.reclaimToSource(bytes32(0));

        assertEq(stable.balanceOf(alice), amount, "minted back");
        assertEq(bnbReceiver.calls(), 0, "no hook");
    }

    function test_RevertWhen_ReclaimTwice() public {
        receiver.setRevertOnHook(true);
        _sendAndCall(bnbUsdtBridge, usdt, alice, OUTBE, address(receiver), 100e6);
        outbeUsdt0Bridge.reclaimToSource(bytes32(0));

        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.NoFailedDelivery.selector, bytes32(0)));
        outbeUsdt0Bridge.reclaimToSource(bytes32(0));
        assertEq(usdt.balanceOf(alice), 100e6);
    }

    function test_RevertWhen_ReclaimWithoutRecord() public {
        _sendAndCall(bnbUsdtBridge, usdt, alice, OUTBE, address(receiver), 100e6);

        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.NoFailedDelivery.selector, bytes32(0)));
        outbeUsdt0Bridge.reclaimToSource(bytes32(0));
    }

    function test_RevertWhen_ReceiveAndCallNotSelf() public {
        vm.prank(address(outbeGateway));
        vm.expectRevert(ERC7786TokenBridge.NotSelf.selector);
        outbeUsdt0Bridge.receiveAndCall(address(receiver), BNB, _interop(BNB, alice), 1, abi.encode(uint32(1)));

        assertEq(usdt0.balanceOf(address(receiver)), 0);
    }

    function test_QuoteSend_WithExtraDataDelegatesToBridge() public {
        bnbGateway.setFeeQuote(456);
        assertEq(bnbUsdtBridge.quoteSend(OUTBE, address(receiver), 1, abi.encode(uint32(1)), 500_000), 456);
    }

    /// @dev Sends `amount` from BNB to `to` on Outbe with a hook, checks that the hook failed, and
    ///      checks the record.
    function _assertFailedDelivery(address to) internal {
        uint256 amount = 100e6;
        bytes memory extraData = abi.encode(uint32(1));
        usdt.mint(alice, amount);

        vm.startPrank(alice);
        usdt.approve(address(bnbUsdtBridge), amount);
        vm.expectEmit(address(outbeUsdt0Bridge));
        emit ERC7786TokenBridge.CrosschainDeliveryFailed(
            bytes32(0), BNB, _interop(block.chainid, alice), to, amount, extraData
        );
        bnbUsdtBridge.sendAndCall(OUTBE, to, amount, extraData, 0);
        vm.stopPrank();

        // The source took the tokens. The destination credited nothing.
        assertEq(usdt.balanceOf(alice), 0, "taken on source");
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), amount, "locked on source");
        assertEq(usdt0.balanceOf(to), 0, "no credit");
        assertEq(usdt0.totalSupply(), 0, "nothing minted");
        _assertRecord(outbeUsdt0Bridge, bytes32(0), BNB, alice, to, amount);
    }

    function _assertRecord(
        ERC7786TokenBridge tokenBridge,
        bytes32 receiveId,
        uint32 sourceDomain,
        address sender,
        address to,
        uint256 amount
    ) internal view {
        ERC7786TokenBridge.FailedDelivery memory failed = tokenBridge.failedDeliveries(receiveId);
        assertEq(failed.sourceDomain, sourceDomain, "record domain");
        assertEq(failed.sender, sender, "record sender");
        assertEq(failed.receiver, to, "record receiver");
        assertEq(failed.amount, amount, "record amount");
    }

    function _sendAndCall(
        ERC7786TokenBridge tokenBridge,
        USDT token,
        address from,
        uint32 destinationDomain,
        address to,
        uint256 amount
    ) internal {
        token.mint(from, amount);
        vm.startPrank(from);
        token.approve(address(tokenBridge), amount);
        tokenBridge.sendAndCall(destinationDomain, to, amount, abi.encode(uint32(1)), 0);
        vm.stopPrank();
    }

    function _nextAddress(uint256 skip) internal view returns (address) {
        return vm.computeCreateAddress(address(this), vm.getNonce(address(this)) + skip);
    }

    function _interop(uint256 chainId, address addr) internal pure returns (bytes memory) {
        return InteroperableAddress.formatEvmV1(chainId, addr);
    }
}
