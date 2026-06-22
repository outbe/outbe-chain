// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";
import {ERC7786Router} from "src/ERC7786Router.sol";
import {ERC7786GatewayMock} from "@openzeppelin/contracts/mocks/crosschain/ERC7786GatewayMock.sol";
import {ERC7786RecipientMock} from "@openzeppelin/contracts/mocks/crosschain/ERC7786RecipientMock.sol";
import {IERC7786GatewaySource, IERC7786Recipient, IGatewayQuote} from "src/interfaces/IERC7786.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";

/// @dev Concrete instance of the abstract OZ loopback gateway, plus a fixed quote for the facade passthrough test.
contract GatewayMock is ERC7786GatewayMock, IGatewayQuote {
    function quote(bytes calldata, bytes calldata) external pure returns (uint256) {
        return 4242;
    }
}

contract ERC7786RouterTest is Test {
    ERC7786Router internal router;

    address internal owner = makeAddr("owner");
    address internal app = makeAddr("app");
    address internal gw = makeAddr("gateway");
    address internal sourceRouter = makeAddr("sourceRouter");

    function setUp() public {
        router = new ERC7786Router(owner, gw);
        // The source-chain counterpart this router trusts on inbound.
        vm.prank(owner);
        router.registerRemoteRouter(_interop(sourceRouter));
    }

    // --------------------------------------------------- helpers ---------------------------------------------------

    function _interop(address a) internal view returns (bytes memory) {
        return InteroperableAddress.formatEvmV1(block.chainid, a);
    }

    function _noAttrs() internal pure returns (bytes[] memory) {
        return new bytes[](0);
    }

    /// @dev Build the payload exactly as {ERC7786Router.sendMessage} wraps it.
    function _wrap(uint256 nonce, address originalSender, address finalRecipient, bytes memory innerPayload)
        internal
        view
        returns (bytes memory)
    {
        return abi.encode(nonce, _interop(originalSender), _interop(finalRecipient), innerPayload);
    }

    // ============================================ sendMessage (outbound) ============================================

    function test_SendMessage_ForwardsThroughActiveGatewayAndRoundTrips() public {
        // Two routers + one loopback gateway, all on this chain, simulating source <-> destination.
        GatewayMock gateway = new GatewayMock();
        ERC7786Router routerA = new ERC7786Router(owner, address(gateway));
        ERC7786Router routerB = new ERC7786Router(owner, address(gateway));
        ERC7786RecipientMock recipient = new ERC7786RecipientMock(address(routerB));

        vm.startPrank(owner);
        routerA.registerRemoteRouter(_interop(address(routerB)));
        routerB.registerRemoteRouter(_interop(address(routerA)));
        vm.stopPrank();

        bytes memory payload = abi.encode("hello", uint256(42));

        vm.recordLogs();
        vm.prank(app);
        routerA.sendMessage(_interop(address(recipient)), payload, _noAttrs());

        // The final recipient must have received the original sender and the unwrapped payload.
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bytes32 topic = keccak256("MessageReceived(address,bytes32,bytes,bytes,uint256)");
        bool seen;
        for (uint256 i = 0; i < logs.length; ++i) {
            if (logs[i].emitter != address(recipient) || logs[i].topics[0] != topic) continue;
            (,, bytes memory gotSender, bytes memory gotPayload,) =
                abi.decode(logs[i].data, (address, bytes32, bytes, bytes, uint256));
            assertEq(gotSender, _interop(app), "original sender preserved");
            assertEq(gotPayload, payload, "payload delivered unwrapped");
            seen = true;
        }
        assertTrue(seen, "recipient should have received the message");
    }

    function test_RevertWhen_SendWithoutActiveGateway() public {
        ERC7786Router noGateway = new ERC7786Router(owner, address(0));
        vm.prank(app);
        vm.expectRevert(ERC7786Router.NoActiveGateway.selector);
        noGateway.sendMessage(_interop(address(0xBEEF)), "x", _noAttrs());
    }

    function test_RevertWhen_SendToUnregisteredRemote() public {
        // Fresh router with a gateway but no registered remotes: lookup of the destination router reverts.
        ERC7786Router fresh = new ERC7786Router(owner, gw);
        vm.prank(app);
        vm.expectRevert();
        fresh.sendMessage(_interop(address(0xBEEF)), "x", _noAttrs());
    }

    function test_SendMessage_ForwardsNativeValue() public {
        // Fee-bearing transports need the native fee funded at send: the router must forward msg.value to the
        // gateway, not reject or hold it. With the loopback gateway the value traverses routerA -> gateway -> routerB.
        GatewayMock gateway = new GatewayMock();
        ERC7786Router routerA = new ERC7786Router(owner, address(gateway));
        ERC7786Router routerB = new ERC7786Router(owner, address(gateway));
        ERC7786RecipientMock recipient = new ERC7786RecipientMock(address(routerB));

        vm.startPrank(owner);
        routerA.registerRemoteRouter(_interop(address(routerB)));
        routerB.registerRemoteRouter(_interop(address(routerA)));
        vm.stopPrank();

        uint256 fee = 0.1 ether;
        vm.deal(app, fee);
        vm.prank(app);
        routerA.sendMessage{value: fee}(_interop(address(recipient)), abi.encode("hello"), _noAttrs());

        assertEq(address(routerB).balance, fee, "native fee forwarded through the router");
    }

    function test_RevertWhen_SendWithAttributes() public {
        bytes[] memory attrs = new bytes[](1);
        attrs[0] = hex"12345678";
        vm.prank(app);
        vm.expectRevert(abi.encodeWithSelector(IERC7786GatewaySource.UnsupportedAttribute.selector, bytes4(0x12345678)));
        router.sendMessage(_interop(sourceRouter), "x", attrs);
    }

    function test_Quote_DelegatesToActiveGateway() public {
        GatewayMock gateway = new GatewayMock();
        ERC7786Router r = new ERC7786Router(owner, address(gateway));
        vm.prank(owner);
        r.registerRemoteRouter(_interop(sourceRouter));

        assertEq(r.quote(_interop(sourceRouter), "payload"), 4242, "facade delegates quote to active gateway");
    }

    function test_RevertWhen_QuoteWithoutActiveGateway() public {
        ERC7786Router noGateway = new ERC7786Router(owner, address(0));
        vm.expectRevert(ERC7786Router.NoActiveGateway.selector);
        noGateway.quote(_interop(sourceRouter), "payload");
    }

    // =========================================== receiveMessage (inbound) ===========================================

    function test_ReceiveMessage_ExecutesOnRecipient() public {
        ERC7786RecipientMock recipient = new ERC7786RecipientMock(address(router));
        bytes memory payload = _wrap(1, app, address(recipient), abi.encode("inner"));

        vm.prank(gw);
        bytes4 magic = router.receiveMessage(bytes32(0), _interop(sourceRouter), payload);
        assertEq(magic, IERC7786Recipient.receiveMessage.selector, "router returns 7786 magic value");
    }

    function test_RevertWhen_ReceiveFromNonActiveGateway() public {
        bytes memory payload = _wrap(1, app, address(0xCAFE), abi.encode("inner"));
        address intruder = makeAddr("intruder");
        vm.prank(intruder);
        vm.expectRevert(abi.encodeWithSelector(ERC7786Router.UnauthorizedGateway.selector, intruder));
        router.receiveMessage(bytes32(0), _interop(sourceRouter), payload);
    }

    function test_RevertWhen_ReceiveFromUnregisteredRemote() public {
        // Sender on a registered EVM chain but a different (unregistered) router address.
        address wrongRemote = makeAddr("wrongRemote");
        bytes memory payload = _wrap(1, app, address(0xCAFE), abi.encode("inner"));
        vm.prank(gw);
        vm.expectRevert(ERC7786Router.InvalidCrosschainSender.selector);
        router.receiveMessage(bytes32(0), _interop(wrongRemote), payload);
    }

    function test_RevertWhen_ReceiveAlreadyExecuted() public {
        ERC7786RecipientMock recipient = new ERC7786RecipientMock(address(router));
        bytes memory payload = _wrap(1, app, address(recipient), abi.encode("inner"));

        vm.prank(gw);
        router.receiveMessage(bytes32(0), _interop(sourceRouter), payload);

        vm.prank(gw);
        vm.expectRevert(ERC7786Router.AlreadyExecuted.selector);
        router.receiveMessage(bytes32(0), _interop(sourceRouter), payload);
    }

    // ============================================ gateway switch (core) =============================================

    function test_SetActiveGateway_SwitchesTrustedInboundGateway() public {
        ERC7786RecipientMock recipient = new ERC7786RecipientMock(address(router));
        address gw2 = makeAddr("gateway2");

        // Old gateway works initially.
        bytes memory p1 = _wrap(1, app, address(recipient), abi.encode("a"));
        vm.prank(gw);
        router.receiveMessage(bytes32(0), _interop(sourceRouter), p1);

        // Switch the active gateway (simulating LZ -> Hyperlane swap).
        vm.prank(owner);
        router.setActiveGateway(gw2);
        assertEq(router.activeGateway(), gw2, "active gateway updated");

        // Old gateway is now rejected.
        bytes memory p2 = _wrap(2, app, address(recipient), abi.encode("b"));
        vm.prank(gw);
        vm.expectRevert(abi.encodeWithSelector(ERC7786Router.UnauthorizedGateway.selector, gw));
        router.receiveMessage(bytes32(0), _interop(sourceRouter), p2);

        // New gateway is accepted.
        vm.prank(gw2);
        bytes4 magic = router.receiveMessage(bytes32(0), _interop(sourceRouter), p2);
        assertEq(magic, IERC7786Recipient.receiveMessage.selector, "new gateway delivers");
    }

    // ================================================ access / pause ================================================

    function test_RevertWhen_NonOwnerSetsGateway() public {
        vm.prank(app);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, app));
        router.setActiveGateway(gw);
    }

    function test_RevertWhen_SendWhilePaused() public {
        vm.prank(owner);
        router.pause();
        vm.prank(app);
        vm.expectRevert(Pausable.EnforcedPause.selector);
        router.sendMessage(_interop(sourceRouter), "x", _noAttrs());
    }

    function test_RevertWhen_ReceiveWhilePaused() public {
        ERC7786RecipientMock recipient = new ERC7786RecipientMock(address(router));
        bytes memory payload = _wrap(1, app, address(recipient), abi.encode("inner"));
        vm.prank(owner);
        router.pause();
        vm.prank(gw);
        vm.expectRevert(Pausable.EnforcedPause.selector);
        router.receiveMessage(bytes32(0), _interop(sourceRouter), payload);
    }
}
