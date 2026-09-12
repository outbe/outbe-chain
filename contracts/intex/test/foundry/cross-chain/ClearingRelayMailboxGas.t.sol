// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockERC7786Bridge} from "@test-mocks/MockERC7786Bridge.sol";
import {ERC7786Bridge} from "@crosschain/ERC7786Bridge.sol";
import {HyperlaneGatewayAdapter} from "@crosschain/adapters/HyperlaneGatewayAdapter.sol";

/// @dev Bids live in storage and are seeded in `setUp`, so the relay reads them cold as in production;
///      `GasBudget.t.sol`'s stub fabricates them in memory and misses two slots per bid.
contract StoredBidStub {
    IIntexAuction.SubmittedBidData[] private _bids;
    uint256 private _visible;

    function seed(uint256 count) external {
        for (uint256 i = 0; i < count; ++i) {
            _bids.push(
                IIntexAuction.SubmittedBidData({
                    bidderAddress: address(uint160(0xCAFE + i)),
                    intexBidRate: 100e6,
                    intexQuantity: 1,
                    timestamp: uint32(block.timestamp),
                    issuanceCurrency: 840,
                    referenceCurrency: 840
                })
            );
        }
    }

    function setVisible(uint256 count) external {
        _visible = count;
    }

    function startClearingStage(uint32) external {}

    function getAuctionDetails(uint32)
        external
        view
        returns (IIntexAuction.AuctionData memory data, IIntexAuction.SubmittedBidData[] memory bids)
    {
        data;
        uint256 n = _visible;
        bids = new IIntexAuction.SubmittedBidData[](n);
        for (uint256 i = 0; i < n; ++i) {
            bids[i] = _bids[i];
        }
    }
}

/// @notice What one bids relay costs on a target chain, per bid count. Run with `--isolate`.
/// @dev Budgets are cut from the fixed part, the step 64 -> 65 (one chunk) and the step 1 -> 64 (per bid).
abstract contract RelayGasBase is Test {
    /// @dev The canonical IGP prices this domain, which `quoteDispatch` needs.
    uint32 internal constant DST_CHAIN_ID = 56;
    uint32 internal constant WORLDWIDE_DAY = 20260501;
    uint256 internal constant SEEDED_BIDS = 256;

    TargetRouter internal router;
    StoredBidStub internal stub;
    address internal admin = address(this);
    address internal originPeer = address(0x0B1);

    function _interop(uint32 chainId, address a) internal pure returns (bytes memory) {
        return InteroperableAddress.formatEvmV1(chainId, a);
    }

    function _measure(uint256 bids) internal returns (uint256 spent) {
        stub.setVisible(bids);
        vm.prank(address(router));
        uint256 before = gasleft();
        router.relayBidsToOutbe(WORLDWIDE_DAY);
        spent = before - gasleft();
    }

    function _wireRouter(address bridge) internal {
        router = DeployProxy.targetRouter(bridge, admin, DST_CHAIN_ID);
        router.setRemoteMessenger(DST_CHAIN_ID, _interop(DST_CHAIN_ID, originPeer));
        stub = new StoredBidStub();
        stub.seed(SEEDED_BIDS);
        router.wire(address(stub), makeAddr("intex"), makeAddr("escrow"));
        vm.deal(address(router), 100 ether);
    }

    function test_Relay0Bids() public {
        emit log_named_uint(string.concat(_label(), "_0bids"), _measure(0));
    }

    function test_Relay1Bid() public {
        emit log_named_uint(string.concat(_label(), "_1bid"), _measure(1));
    }

    function test_Relay64Bids() public {
        emit log_named_uint(string.concat(_label(), "_64bids"), _measure(64));
    }

    function test_Relay65Bids() public {
        emit log_named_uint(string.concat(_label(), "_65bids"), _measure(65));
    }

    function test_Relay128Bids() public {
        emit log_named_uint(string.concat(_label(), "_128bids"), _measure(128));
    }

    function test_Relay256Bids() public {
        emit log_named_uint(string.concat(_label(), "_256bids"), _measure(256));
    }

    function _label() internal pure virtual returns (string memory);
}

/// @notice The relay against the canonical Hyperlane mailbox, over a mainnet fork.
/// @dev A real `dispatch` inserts into the live merkle tree, runs the default hook and pays the IGP;
///      the mock does none of it. Skips without an endpoint, so CI stays offline.
contract ClearingRelayRealMailboxTest is RelayGasBase {
    address internal constant MAILBOX = 0xc005dc82818d67AF737725bD4bf75435d065D239;

    HyperlaneGatewayAdapter internal adapter;
    ERC7786Bridge internal hub;

    function setUp() public {
        string memory rpc = vm.envOr("ETH_MAINNET_RPC_URL", string(""));
        if (bytes(rpc).length == 0) {
            vm.skip(true);
            return;
        }
        vm.createSelectFork(rpc);

        adapter = new HyperlaneGatewayAdapter(MAILBOX, admin);
        hub = new ERC7786Bridge(admin, address(adapter));
        hub.setGateway(DST_CHAIN_ID, address(adapter));
        hub.registerRemoteBridge(_interop(DST_CHAIN_ID, makeAddr("remoteHub")));
        adapter.setRouterWithChain(DST_CHAIN_ID, bytes32(uint256(uint160(makeAddr("remoteAdapter")))), DST_CHAIN_ID);

        _wireRouter(address(hub));
    }

    function _label() internal pure override returns (string memory) {
        return "relay_real";
    }

    /// @dev The whole delivery, entered where the mailbox enters it. The budget covers this; the step
    ///      down to `relay_real_*` is the preamble the gas floor must allow for.
    function _measureDelivery(uint256 bids) internal returns (uint256 spent) {
        stub.setVisible(bids);
        bytes memory wrapped = abi.encode(
            uint256(1),
            _interop(DST_CHAIN_ID, originPeer),
            _interop(uint32(block.chainid), address(router)),
            BridgeMsgCodec.encodeAuctionStageClearing(WORLDWIDE_DAY)
        );
        bytes memory adapterMessage = abi.encode(
            _interop(DST_CHAIN_ID, makeAddr("remoteHub")), _interop(uint32(block.chainid), address(hub)), wrapped
        );

        vm.prank(MAILBOX);
        uint256 before = gasleft();
        adapter.handle(DST_CHAIN_ID, bytes32(uint256(uint160(makeAddr("remoteAdapter")))), adapterMessage);
        spent = before - gasleft();
    }

    function test_Delivery0Bids() public {
        emit log_named_uint("delivery_real_0bids", _measureDelivery(0));
    }

    function test_Delivery64Bids() public {
        emit log_named_uint("delivery_real_64bids", _measureDelivery(64));
    }

    function test_Delivery256Bids() public {
        emit log_named_uint("delivery_real_256bids", _measureDelivery(256));
    }
}

/// @notice The same relay through the mock, to size the stand's distortion.
/// @dev The mock stores the message body in `lastPayload` - 22 100 per fresh word - which dominates the
///      first chunk and inflates `GasBudget.t.sol`'s clearing curve.
contract ClearingRelayMockBridgeTest is RelayGasBase {
    function setUp() public {
        MockERC7786Bridge bridge = new MockERC7786Bridge();
        bridge.setAutoDeliver(false);
        _wireRouter(address(bridge));
    }

    function _label() internal pure override returns (string memory) {
        return "relay_mock";
    }
}
