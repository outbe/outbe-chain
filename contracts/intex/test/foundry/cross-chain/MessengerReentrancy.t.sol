// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MessagingFee, Origin} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";
import {Vm} from "forge-std/Vm.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";

import {TargetMessenger} from "@contracts/target/TargetMessenger.sol";
import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IDesis} from "@contracts/origin/interfaces/IDesis.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @dev Selector of OZ `ReentrancyGuardReentrantCall()`, reverted by `nonReentrant` on re-entry.
bytes4 constant REENTRANCY_GUARD_REENTRANT_CALL = 0x3ee5aeb5;

Vm constant VM = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

/// @dev Re-enters the messenger's endpoint-gated `lzReceive` while the outer `_lzReceive` still holds
///      the guard. Returns true iff that re-entry reverts with `ReentrancyGuardReentrantCall` — the
///      observable signature of the `nonReentrant` modifier on the inbound entry. `vm.prank(endpoint)`
///      clears the `onlyEndpoint` gate so the guard (not the gate) is what rejects the call. Replaces
///      the old storage-slot probe, which cannot read the now-transient guard across contracts.
function reentryGuarded(address messenger, address endpoint, uint32 srcEid, bytes32 peer) returns (bool) {
    Origin memory origin = Origin({srcEid: srcEid, sender: peer, nonce: 2});
    VM.prank(endpoint);
    (bool ok, bytes memory ret) = messenger.call(
        abi.encodeWithSignature(
            "lzReceive((uint32,bytes32,uint64),bytes32,bytes,address,bytes)",
            origin,
            bytes32(0),
            bytes(""),
            address(0),
            bytes("")
        )
    );
    return !ok && ret.length >= 4 && bytes4(ret) == REENTRANCY_GUARD_REENTRANT_CALL;
}

/// @notice Stub Auction that, during the inbound STAGE_START path, tries to re-enter the messenger's
///         inbound entry. The re-entry reverts iff `_lzReceive` carries `nonReentrant`.
/// @dev Does NOT inherit `IIntexAuction` — the high-level call dispatches by selector, so matching
///      the `auctionStart` signature here is sufficient.
contract ReentrancyProbeAuction {
    address public immutable messenger;
    address public immutable endpoint;
    uint32 public immutable srcEid;
    bytes32 public immutable peer;
    bool public observed;
    bool public guardHeld;

    constructor(address messenger_, address endpoint_, uint32 srcEid_, address peer_) {
        messenger = messenger_;
        endpoint = endpoint_;
        srcEid = srcEid_;
        peer = bytes32(uint256(uint160(peer_)));
    }

    function auctionStart(uint32, IIntexAuction.AuctionSchedule calldata, IIntexAuction.AuctionParams calldata)
        external
    {
        observed = true;
        guardHeld = reentryGuarded(messenger, endpoint, srcEid, peer);
    }
}

/// @notice Stub Desis that tries to re-enter OM's inbound entry during BIDS_BATCH dispatch.
contract ReentrancyProbeDesis {
    address public immutable messenger;
    address public immutable endpoint;
    uint32 public immutable srcEid;
    bytes32 public immutable peer;
    bool public observed;
    bool public guardHeld;

    constructor(address messenger_, address endpoint_, uint32 srcEid_, address peer_) {
        messenger = messenger_;
        endpoint = endpoint_;
        srcEid = srcEid_;
        peer = bytes32(uint256(uint160(peer_)));
    }

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == type(IDesis).interfaceId || interfaceId == type(IERC165).interfaceId;
    }

    function processBidsBatch(
        uint32,
        uint32,
        bool,
        uint32,
        address[] calldata,
        uint16[] calldata,
        uint32[] calldata,
        uint32[] calldata
    ) external {
        observed = true;
        guardHeld = reentryGuarded(messenger, endpoint, srcEid, peer);
    }

    /// @dev OriginMessenger reads stage post-processBidsBatch to decide on auto-clear.
    ///      Return `None` so the auto-clear branch is skipped in this probe-only test.
    function getAuctionStage(uint32) external pure returns (IDesis.AuctionStage) {
        return IDesis.AuctionStage.None;
    }
}

/// @title MessengerReentrancyTest
/// @notice Behavioural test that `TargetMessenger._lzReceive` and `OriginMessenger._lzReceive`
///         run under OZ `nonReentrant`.
/// @dev Same observation pattern used for `ONFT1155Adapter`: a downstream callee reads the
///      bridge's guard slot mid-call. The slot reads `ENTERED (2)` iff the `nonReentrant`
///      modifier is active on the inbound entry. We can't behaviourally observe
///      `ReentrancyGuardReentrantCall` without bypassing the endpoint-only gate; this storage
///      probe pins the visible signature of the modifier.
contract MessengerReentrancyTest is TestHelperOz5 {
    using OptionsBuilder for bytes;

    uint32 internal constant BNB_EID = 1;
    uint32 internal constant OUTBE_EID = 2;
    bytes32 internal constant DUMMY_GUID = bytes32(uint256(0xCAFE));

    TargetMessenger internal bnbMessenger;
    OriginMessenger internal outbeMessenger;

    address internal admin = address(this);

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        bnbMessenger = DeployProxy.targetMessenger(address(endpoints[BNB_EID]), admin, OUTBE_EID);
        outbeMessenger = DeployProxy.originMessenger(address(endpoints[OUTBE_EID]), admin, BNB_EID);

        address[] memory bridge = new address[](2);
        bridge[0] = address(bnbMessenger);
        bridge[1] = address(outbeMessenger);
        this.wireOApps(bridge);
    }

    function _deliver(address oapp, address endpointAddr, uint32 srcEid, address peer, bytes memory message) internal {
        Origin memory origin = Origin({srcEid: srcEid, sender: bytes32(uint256(uint160(peer))), nonce: 1});
        vm.prank(endpointAddr);
        (bool ok, bytes memory data) = oapp.call(
            abi.encodeWithSignature(
                "lzReceive((uint32,bytes32,uint64),bytes32,bytes,address,bytes)",
                origin,
                DUMMY_GUID,
                message,
                address(0),
                ""
            )
        );
        if (!ok) {
            assembly {
                revert(add(data, 32), mload(data))
            }
        }
    }

    function test_TM_lzReceive_runsUnderNonReentrant() public {
        ReentrancyProbeAuction probeAuction = new ReentrancyProbeAuction(
            address(bnbMessenger), address(endpoints[BNB_EID]), OUTBE_EID, address(outbeMessenger)
        );
        // intex / escrow / onftBatch don't fire on STAGE_START, can be the zero-stub probe too,
        // but `wire` rejects address(0). Reuse the probe so all four wires are non-zero.
        bnbMessenger.wire(address(probeAuction), address(probeAuction), address(probeAuction), address(probeAuction));

        bytes memory packet =
            BridgeMsgCodec.encodeAuctionStageStart(42, 100, 200, 300, 840, 840, 1e18, 5e6, 7e6, 11e6, 4e6, 5, 6, 7, 3);

        _deliver(address(bnbMessenger), address(endpoints[BNB_EID]), OUTBE_EID, address(outbeMessenger), packet);

        assertTrue(probeAuction.observed(), "TM auction callback never ran");
        assertTrue(probeAuction.guardHeld(), "TargetMessenger._lzReceive missing nonReentrant");
    }

    function test_OM_lzReceive_runsUnderNonReentrant() public {
        ReentrancyProbeDesis probeDesis = new ReentrancyProbeDesis(
            address(outbeMessenger), address(endpoints[OUTBE_EID]), BNB_EID, address(bnbMessenger)
        );
        outbeMessenger.wire(address(probeDesis), makeAddr("factory"));

        bytes memory packet = BridgeMsgCodec.encodeBidsBatch(
            42, BNB_EID, true, 1, new address[](0), new uint16[](0), new uint32[](0), new uint32[](0)
        );

        _deliver(address(outbeMessenger), address(endpoints[OUTBE_EID]), BNB_EID, address(bnbMessenger), packet);

        assertTrue(probeDesis.observed(), "OM Desis callback never ran");
        assertTrue(probeDesis.guardHeld(), "OriginMessenger._lzReceive missing nonReentrant");
    }
}
