// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {Vm} from "forge-std/Vm.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {TargetMessenger} from "@contracts/target/TargetMessenger.sol";
import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IDesis} from "@contracts/origin/interfaces/IDesis.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @dev Selector of OZ `ReentrancyGuardReentrantCall()`, reverted by `nonReentrant` on re-entry.
bytes4 constant REENTRANCY_GUARD_REENTRANT_CALL = 0x3ee5aeb5;

Vm constant VM = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

/// @dev Re-enters the messenger's bridge-gated inbound entry (`receiveMessage`) while the outer dispatch still
///      holds the guard, by re-delivering an empty message through the loopback bridge. Returns true iff that
///      re-entry reverts with `ReentrancyGuardReentrantCall` — the observable signature of the `nonReentrant`
///      modifier on `receiveMessage`. Going through the bridge clears the `UnauthorizedBridge` gate so the guard
///      (not the caller check) is what rejects the call; the empty payload can never be reached because the guard
///      fires first.
function reentryGuarded(address bridge, uint32 srcChainId, address peer, address messenger) returns (bool) {
    bytes memory sender = InteroperableAddress.formatEvmV1(srcChainId, peer);
    bytes memory recipient = InteroperableAddress.formatEvmV1(uint32(block.chainid), messenger);
    (bool ok, bytes memory ret) =
        bridge.call(abi.encodeWithSignature("deliverAs(bytes,bytes,bytes)", sender, recipient, bytes("")));
    return !ok && ret.length >= 4 && bytes4(ret) == REENTRANCY_GUARD_REENTRANT_CALL;
}

/// @notice Stub Auction that, during the inbound STAGE_START dispatch, tries to re-enter the messenger's inbound
///         entry. The re-entry reverts iff `receiveMessage` carries `nonReentrant`.
/// @dev Does NOT inherit `IIntexAuction` — the high-level call dispatches by selector, so matching the
///      `auctionStart` signature here is sufficient.
contract ReentrancyProbeAuction {
    address public immutable bridge;
    uint32 public immutable srcChainId;
    address public immutable peer;
    address public immutable messenger;
    bool public observed;
    bool public guardHeld;

    constructor(address bridge_, uint32 srcChainId_, address peer_, address messenger_) {
        bridge = bridge_;
        srcChainId = srcChainId_;
        peer = peer_;
        messenger = messenger_;
    }

    function auctionStart(uint32, IIntexAuction.AuctionSchedule calldata, IIntexAuction.AuctionParams calldata)
        external
    {
        observed = true;
        guardHeld = reentryGuarded(bridge, srcChainId, peer, messenger);
    }
}

/// @notice Stub Desis that tries to re-enter OM's inbound entry during BIDS_BATCH dispatch.
contract ReentrancyProbeDesis {
    address public immutable bridge;
    uint32 public immutable srcChainId;
    address public immutable peer;
    address public immutable messenger;
    bool public observed;
    bool public guardHeld;

    constructor(address bridge_, uint32 srcChainId_, address peer_, address messenger_) {
        bridge = bridge_;
        srcChainId = srcChainId_;
        peer = peer_;
        messenger = messenger_;
    }

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == type(IDesis).interfaceId || interfaceId == type(IERC165).interfaceId;
    }

    function processBidsBatch(
        uint32,
        uint32,
        uint32,
        uint16,
        uint16,
        address[] calldata,
        uint16[] calldata,
        uint32[] calldata,
        uint32[] calldata
    ) external {
        observed = true;
        guardHeld = reentryGuarded(bridge, srcChainId, peer, messenger);
    }

    /// @dev OriginMessenger reads stage post-processBidsBatch to decide on auto-clear.
    ///      Return `None` so the auto-clear branch is skipped in this probe-only test.
    function getAuctionStage(uint32) external pure returns (IDesis.AuctionStage) {
        return IDesis.AuctionStage.None;
    }
}

/// @title MessengerReentrancyTest
/// @notice Behavioural test that `TargetMessenger.receiveMessage` and `OriginMessenger.receiveMessage` run under
///         OZ `nonReentrant`.
/// @dev A downstream callee (auction/desis) tries to re-enter the messenger's inbound entry mid-dispatch, through
///      the loopback bridge (so the bridge gate is cleared). The re-entry reverts with `ReentrancyGuardReentrantCall`
///      iff the `nonReentrant` modifier is active on `receiveMessage`.
contract MessengerReentrancyTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    TargetMessenger internal bnbMessenger;
    OriginMessenger internal outbeMessenger;

    address internal admin = address(this);

    function setUp() public {
        _setUpBridge();

        bnbMessenger = DeployProxy.targetMessenger(address(bridge), admin, OUTBE_CHAIN_ID);
        outbeMessenger = DeployProxy.originMessenger(address(bridge), admin, BNB_CHAIN_ID);

        bnbMessenger.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(outbeMessenger)));
        outbeMessenger.setRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(bnbMessenger)));
    }

    function test_TM_receiveMessage_runsUnderNonReentrant() public {
        ReentrancyProbeAuction probeAuction =
            new ReentrancyProbeAuction(address(bridge), OUTBE_CHAIN_ID, address(outbeMessenger), address(bnbMessenger));
        // intex / escrow / onftBatch don't fire on STAGE_START, but `wire` rejects address(0). Reuse the
        // probe so all four wires are non-zero.
        bnbMessenger.wire(address(probeAuction), address(probeAuction), address(probeAuction), address(probeAuction));

        bytes memory packet =
            BridgeMsgCodec.encodeAuctionStageStart(42, 100, 200, 300, 840, 840, 1e18, 5e6, 7e6, 11e6, 4e6, 5, 6, 7, 3);

        _deliver(OUTBE_CHAIN_ID, address(outbeMessenger), address(bnbMessenger), packet);

        assertTrue(probeAuction.observed(), "TM auction callback never ran");
        assertTrue(probeAuction.guardHeld(), "TargetMessenger.receiveMessage missing nonReentrant");
    }

    function test_OM_receiveMessage_runsUnderNonReentrant() public {
        ReentrancyProbeDesis probeDesis =
            new ReentrancyProbeDesis(address(bridge), BNB_CHAIN_ID, address(bnbMessenger), address(outbeMessenger));
        outbeMessenger.wire(address(probeDesis), makeAddr("factory"));

        bytes memory packet = BridgeMsgCodec.encodeBidsBatch(
            42, BNB_CHAIN_ID, 1, 0, 1, new address[](0), new uint16[](0), new uint32[](0), new uint32[](0)
        );

        _deliver(BNB_CHAIN_ID, address(bnbMessenger), address(outbeMessenger), packet);

        assertTrue(probeDesis.observed(), "OM Desis callback never ran");
        assertTrue(probeDesis.guardHeld(), "OriginMessenger.receiveMessage missing nonReentrant");
    }
}
