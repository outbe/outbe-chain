// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {IERC7786GatewaySource} from "@openzeppelin/contracts/interfaces/draft-IERC7786.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IGatewayQuote} from "@contracts/shared/interfaces/IGatewayQuote.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockERC7786Bridge} from "@test-mocks/MockERC7786Bridge.sol";

interface IMailbox {
    function dispatch(uint32 destinationDomain, bytes32 recipient, bytes calldata body, bytes calldata metadata)
        external
        payable
        returns (bytes32);

    function quoteDispatch(uint32 destinationDomain, bytes32 recipient, bytes calldata body, bytes calldata metadata)
        external
        view
        returns (uint256);
}

/// @notice What the hub and its Hyperlane adapter do on the way out, wired to the real mailbox.
/// @dev Reproduced here rather than imported from `contracts/crosschain`: compiling that into this project
///      puts it in its own solc unit, which the upgrades-core layout validator cannot dereference. Both
///      wraps are byte-identical to production, so the mailbox hashes and logs a production-sized message.
contract LocalHyperlaneGateway is IERC7786GatewaySource, IGatewayQuote {
    IMailbox private immutable MAILBOX;
    uint32 private immutable DOMAIN;
    uint256 private _nonce;

    bytes4 private constant GAS_LIMIT_SELECTOR = bytes4(keccak256("executionGasLimit(uint256)"));
    uint16 private constant HOOK_METADATA_VARIANT = 1;
    uint256 private constant DEFAULT_GAS_LIMIT = 200_000;

    constructor(address mailbox, uint32 domain) {
        MAILBOX = IMailbox(mailbox);
        DOMAIN = domain;
    }

    function supportsAttribute(bytes4) external pure returns (bool) {
        return true;
    }

    function quote(bytes calldata recipient, bytes calldata payload) external view returns (uint256) {
        return _quote(recipient, payload, DEFAULT_GAS_LIMIT);
    }

    function quote(bytes calldata recipient, bytes calldata payload, bytes[] calldata attributes)
        external
        view
        returns (uint256)
    {
        return _quote(recipient, payload, _gasAttribute(attributes));
    }

    function sendMessage(bytes calldata recipient, bytes calldata payload, bytes[] calldata attributes)
        external
        payable
        returns (bytes32)
    {
        return MAILBOX.dispatch{value: msg.value}(
            DOMAIN, _self(), _body(recipient, payload, ++_nonce), _metadata(_gasAttribute(attributes))
        );
    }

    /// @dev Hub wrap (nonce, sender, recipient, payload) inside the adapter wrap (sender, recipient, body).
    function _body(bytes calldata recipient, bytes calldata payload, uint256 nonce)
        private
        view
        returns (bytes memory)
    {
        bytes memory sender = InteroperableAddress.formatEvmV1(block.chainid, msg.sender);
        return abi.encode(sender, recipient, abi.encode(nonce, sender, recipient, payload));
    }

    function _quote(bytes calldata recipient, bytes calldata payload, uint256 gasLimit) private view returns (uint256) {
        return MAILBOX.quoteDispatch(DOMAIN, _self(), _body(recipient, payload, _nonce + 1), _metadata(gasLimit));
    }

    function _self() private view returns (bytes32) {
        return bytes32(uint256(uint160(address(this))));
    }

    function _metadata(uint256 gasLimit) private view returns (bytes memory) {
        return abi.encodePacked(HOOK_METADATA_VARIANT, uint256(0), gasLimit, msg.sender);
    }

    function _gasAttribute(bytes[] calldata attributes) private pure returns (uint256) {
        for (uint256 i = 0; i < attributes.length; i++) {
            if (bytes4(attributes[i]) == GAS_LIMIT_SELECTOR) return abi.decode(attributes[i][4:], (uint256));
        }
        return DEFAULT_GAS_LIMIT;
    }
}

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

    function getAuctionStage(uint32) external pure returns (IIntexAuction.AuctionStage) {
        return IIntexAuction.AuctionStage.Issuance;
    }

    function revealedBidsCount(uint32) external view returns (uint256) {
        return _visible;
    }

    function revealedBidsSlice(uint32, uint256 offset, uint256 limit)
        external
        view
        returns (IIntexAuction.SubmittedBidData[] memory slice)
    {
        uint256 length = _visible;
        if (offset >= length) return new IIntexAuction.SubmittedBidData[](0);
        uint256 end = offset + limit;
        if (end > length) end = length;
        slice = new IIntexAuction.SubmittedBidData[](end - offset);
        for (uint256 i = 0; i < slice.length; ++i) {
            slice[i] = _bids[offset + i];
        }
    }
}

/// @notice What one bids relay costs on a target chain, per bid count. Run with `--isolate`.
/// @dev The round budgets are cut from the fixed part, the step 64 -> 65 (one chunk) and the step 1 -> 64
///      (per bid). `_measure` drives `relayBidsToOutbe` as the router itself - the same entry the inbound
///      clearing handler uses - so only the relay is in the reading. Against the real hub and adapter a
///      bid-less relay came out 81k higher over its two sends - their frames plus the hub's nonce write -
///      so the budgets carry ~40k a send on top of what this measures: 564k fixed, ~215k a send, ~8.7k a
///      bid.
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
/// @dev A real `dispatch` inserts into the live merkle tree, runs the default hook and pays the IGP; the
///      mock does none of it. Skips without an endpoint, so CI stays offline.
contract ClearingRelayRealMailboxTest is RelayGasBase {
    address internal constant MAILBOX = 0xc005dc82818d67AF737725bD4bf75435d065D239;

    LocalHyperlaneGateway internal gateway;

    function setUp() public {
        string memory rpc = vm.envOr("ETH_MAINNET_RPC_URL", string(""));
        if (bytes(rpc).length == 0) {
            vm.skip(true);
            return;
        }
        vm.createSelectFork(rpc);

        gateway = new LocalHyperlaneGateway(MAILBOX, DST_CHAIN_ID);
        _wireRouter(address(gateway));
    }

    function _label() internal pure override returns (string memory) {
        return "relay_real";
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
