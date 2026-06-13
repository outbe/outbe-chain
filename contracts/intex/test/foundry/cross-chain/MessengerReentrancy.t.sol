// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MessagingFee, Origin} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";
import {Vm} from "forge-std/Vm.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";

import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";
import {OriginMessenger} from "@contracts/outbe/OriginMessenger.sol";
import {IIntexAuction} from "@contracts/bnb/interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IDesis} from "@contracts/outbe/interfaces/IDesis.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @dev Storage slot of OZ `ReentrancyGuard._status` (ERC-7201).
///      keccak256(abi.encode(uint256(keccak256("openzeppelin.storage.ReentrancyGuard")) - 1)) & ~bytes32(uint256(0xff))
bytes32 constant REENTRANCY_GUARD_STORAGE = 0x9b779b17422d0df92223018b32b4d1fa46e071723d6817e2486d003becc55f00;
uint256 constant ENTERED = 2;

Vm constant VM = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

/// @notice Stub Auction that, during the inbound STAGE_START path, snapshots the bridge's
///         ReentrancyGuard `_status` slot. Reads `ENTERED == 2` iff `_lzReceive` carries
///         `nonReentrant`.
/// @dev Does NOT inherit `IIntexAuction` — the interface has many other methods unrelated to
///      the STAGE_START path. The high-level call from `TargetMessenger` dispatches by selector,
///      so matching the signature here is sufficient.
contract ReentrancyProbeAuction {
    address public immutable bridge;
    uint256 public observedGuardSlot;
    bool public observed;

    constructor(address bridge_) {
        bridge = bridge_;
    }

    function auctionStart(uint32, IIntexAuction.AuctionSchedule calldata, IIntexAuction.AuctionParams calldata)
        external
    {
        observedGuardSlot = uint256(VM.load(bridge, REENTRANCY_GUARD_STORAGE));
        observed = true;
    }
}

/// @notice Stub Desis that snapshots OM's ReentrancyGuard slot during BIDS_BATCH dispatch.
contract ReentrancyProbeDesis {
    address public immutable bridge;
    uint256 public observedGuardSlot;
    bool public observed;

    constructor(address bridge_) {
        bridge = bridge_;
    }

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == type(IDesis).interfaceId || interfaceId == type(IERC165).interfaceId;
    }

    function processBidsBatch(
        uint32, /* seriesId */
        uint32, /* srcEid */
        bool, /* isLast */
        uint32, /* relayGeneration */
        address[] calldata, /* bidders */
        uint16[] calldata, /* quantities */
        uint64[] calldata, /* prices */
        uint32[] calldata /* timestamps */
    ) external {
        observedGuardSlot = uint256(VM.load(bridge, REENTRANCY_GUARD_STORAGE));
        observed = true;
    }

    /// @dev OriginMessenger reads stage post-processBidsBatch to decide on auto-clear.
    ///      Return `None` so the auto-clear branch is skipped in this probe-only test.
    function getAuctionStage(
        uint32 /*seriesId*/
    )
        external
        pure
        returns (IDesis.AuctionStage)
    {
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
        ReentrancyProbeAuction probeAuction = new ReentrancyProbeAuction(address(bnbMessenger));
        // intex / escrow / onftBatch don't fire on STAGE_START, can be the zero-stub probe too,
        // but `wire` rejects address(0). Reuse the probe so all four wires are non-zero.
        bnbMessenger.wire(address(probeAuction), address(probeAuction), address(probeAuction), address(probeAuction));

        bytes memory packet = BridgeMsgCodec.encodeAuctionStageStart(42, 100, 200, 300, 1e18, 5e6, 7e6, 11e6, 3);

        _deliver(address(bnbMessenger), address(endpoints[BNB_EID]), OUTBE_EID, address(outbeMessenger), packet);

        assertTrue(probeAuction.observed(), "TM auction callback never ran");
        assertEq(probeAuction.observedGuardSlot(), ENTERED, "TargetMessenger._lzReceive missing nonReentrant");
    }

    function test_OM_lzReceive_runsUnderNonReentrant() public {
        ReentrancyProbeDesis probeDesis = new ReentrancyProbeDesis(address(outbeMessenger));
        outbeMessenger.wire(address(probeDesis), makeAddr("factory"));

        bytes memory packet = BridgeMsgCodec.encodeBidsBatch(
            42, BNB_EID, true, 1, new address[](0), new uint16[](0), new uint64[](0), new uint32[](0)
        );

        _deliver(address(outbeMessenger), address(endpoints[OUTBE_EID]), BNB_EID, address(bnbMessenger), packet);

        assertTrue(probeDesis.observed(), "OM Desis callback never ran");
        assertEq(probeDesis.observedGuardSlot(), ENTERED, "OriginMessenger._lzReceive missing nonReentrant");
    }
}
