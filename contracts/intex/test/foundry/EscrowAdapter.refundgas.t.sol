// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "./helpers/CrossChainTest.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {ITheCompact} from "@contracts/vendor/the-compact/interfaces/ITheCompact.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

/// @dev Takes the tokens the way a composed-transfer bridge does, and nothing else.
contract PullingTokenBridge {
    IERC20 private immutable TOKEN;

    constructor(IERC20 token) {
        TOKEN = token;
    }

    function quoteSend(uint32, address, uint256, bytes calldata, uint256) external pure returns (uint256) {
        return 0.001 ether;
    }

    function sendAndCall(uint32, address, uint256 amount, bytes calldata, uint256) external payable returns (bytes32) {
        TOKEN.transferFrom(msg.sender, address(this), amount);
        return bytes32(uint256(1));
    }
}

/// @notice What the refund path costs against the canonical Compact, brought up from the genesis predeploy's
///         runtime code so it needs no RPC. One measurement per test with state seeded in `setUp`; run with
///         `--isolate`.
abstract contract RefundGasBase is CrossChainTest {
    address internal constant COMPACT = 0x00000000000000171ede64904551eeDF3C6C9788;
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant DAY = 20260501;
    uint128 internal constant LOCK = 1000e18;

    EscrowAdapter internal escrow;
    IERC20 internal token;
    address internal admin = address(this);

    function _setUpEscrow() internal {
        vm.etch(COMPACT, vm.parseBytes(vm.trim(vm.readFile("../../scripts/contracts/the_compact.code.hex"))));
        token = IERC20(address(new MockWCOEN()));
        escrow = DeployProxy.escrowAdapter(admin, admin);
        escrow.wire(admin, COMPACT, address(token));
        escrow.grantRole(escrow.AUCTION_ROLE(), admin);
        escrow.grantRole(escrow.RELAYER_ROLE(), admin);
        escrow.setProceedsRecipient(makeAddr("proceeds"));
    }

    function _bidder(uint256 i) internal pure returns (address) {
        return address(uint160(0x5000 + i));
    }

    /// @dev `held` stays on each bidder's balance after locking, so a refund lands on a balance already in use.
    function _lock(uint256 from, uint256 count, uint128 amount, uint128 held) internal {
        for (uint256 i = from; i < from + count; ++i) {
            address who = _bidder(i);
            deal(address(token), who, uint256(amount) + held);
            vm.prank(who);
            token.approve(address(escrow), type(uint256).max);
            escrow.lockFunds(DAY, who, amount, 1_000_000, 1);
        }
    }

    function _elapseCompactResetPeriod() internal {
        vm.warp(block.timestamp + 5 minutes);
    }

    function _finalize(uint256 n, uint128 paid) internal returns (uint256 spent) {
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](n);
        for (uint256 i = 0; i < n; ++i) {
            instructions[i] = IEscrowAdapter.FinalizationInstruction({
                bidder: _bidder(i), refundedAmount: LOCK - paid, paidAmount: paid
            });
        }
        uint256 before = gasleft();
        escrow.finalizeAuction(DAY, bytes32(uint256(1)), instructions, false);
        spent = before - gasleft();
    }
}

contract RefundFinalizeGasTest is RefundGasBase {
    function setUp() public {
        _setUpEscrow();
        _lock(0, 64, LOCK, 0);
        _elapseCompactResetPeriod();
    }

    function test_Finalize1Loser() public {
        emit log_named_uint("finalize_losers_1", _finalize(1, 0));
    }

    function test_Finalize2Losers() public {
        emit log_named_uint("finalize_losers_2", _finalize(2, 0));
    }

    function test_Finalize8Losers() public {
        emit log_named_uint("finalize_losers_8", _finalize(8, 0));
    }

    function test_Finalize32Losers() public {
        emit log_named_uint("finalize_losers_32", _finalize(32, 0));
    }

    function test_Finalize64Losers() public {
        emit log_named_uint("finalize_losers_64", _finalize(64, 0));
    }

    /// @dev Nothing to refund, so the per-bidder transfer is skipped and only the withdrawal remains.
    function test_Finalize64Winners() public {
        emit log_named_uint("finalize_winners_64", _finalize(64, LOCK));
    }
}

contract RefundFinalizeHeldBalanceGasTest is RefundGasBase {
    function setUp() public {
        _setUpEscrow();
        _lock(0, 64, LOCK, 1e18);
        _elapseCompactResetPeriod();
    }

    function test_Finalize64LosersHoldingTheToken() public {
        emit log_named_uint("finalize_losers_64_held", _finalize(64, 0));
    }
}

contract RefundClaimUnfinalizedGasTest is RefundGasBase {
    function setUp() public {
        _setUpEscrow();
        _lock(0, 1, LOCK, 0);
        vm.warp(block.timestamp + escrow.UNFINALIZED_REFUND_DELAY());
    }

    function test_ClaimOnADayThatNeverFinalized() public {
        uint256 before = gasleft();
        escrow.claimRefund(DAY, _bidder(0));
        emit log_named_uint("claim_unfinalized", before - gasleft());
    }
}

/// @dev Bidder 0 is left out of the finalization and bidder 1's instruction fails with its split recorded.
contract RefundClaimFinalizedGasTest is RefundGasBase {
    function setUp() public {
        _setUpEscrow();
        _lock(0, 1, LOCK, 0);
        _lock(1, 1, LOCK + 1, 0);
        _elapseCompactResetPeriod();

        vm.mockCallRevert(
            COMPACT,
            abi.encodeCall(ITheCompact.forcedWithdrawal, (escrow.lockId(), address(escrow), uint256(LOCK) + 1)),
            ""
        );
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: _bidder(1), refundedAmount: LOCK / 2 + 1, paidAmount: LOCK / 2
        });
        escrow.finalizeAuction(DAY, bytes32(uint256(1)), instructions, true);
        vm.clearMockedCalls();

        vm.warp(block.timestamp + escrow.POST_FINALIZE_REFUND_DELAY());
    }

    function test_ClaimAsAnOmittedBidder() public {
        uint256 before = gasleft();
        escrow.claimRefund(DAY, _bidder(0));
        emit log_named_uint("claim_finalized_omitted", before - gasleft());
    }

    function test_ClaimARecordedSplit() public {
        uint256 before = gasleft();
        escrow.claimRefund(DAY, _bidder(1));
        emit log_named_uint("claim_finalized_split", before - gasleft());
    }
}

/// @notice A day of 200 bidders with 40 winners, as it reaches a target chain today: four chunks of 64, 64, 64
///         and 8. The day costs the first chunk, two middle ones and the closing one.
abstract contract RefundDayGasBase is RefundGasBase {
    uint256 internal constant BIDDERS = 200;
    uint256 internal constant WINNERS = 40;
    uint256 internal constant CHUNK = 64;
    uint16 internal constant CHUNKS = 4;

    TargetRouter internal router;
    address internal originPeer = address(0x0B1);

    function _setUpDay(uint16 chunksDelivered) internal {
        _setUpBridge();
        _setUpEscrow();

        router = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        router.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, originPeer));
        router.wire(makeAddr("auction"), makeAddr("intex"), address(escrow));
        router.setProceedsRoute(address(new PullingTokenBridge(token)), makeAddr("originRouter"));
        escrow.setProceedsRecipient(address(router));
        escrow.grantRole(escrow.RELAYER_ROLE(), address(router));
        vm.deal(address(router), 1 ether);

        _lock(0, BIDDERS, LOCK, 0);
        _elapseCompactResetPeriod();

        for (uint16 i = 0; i < chunksDelivered; ++i) {
            _deliver(OUTBE_CHAIN_ID, originPeer, address(router), _chunk(i));
        }
    }

    function _chunk(uint16 index) internal pure returns (bytes memory) {
        uint256 start = uint256(index) * CHUNK;
        uint256 end = start + CHUNK > BIDDERS ? BIDDERS : start + CHUNK;
        address[] memory who = new address[](end - start);
        uint128[] memory refunded = new uint128[](end - start);
        uint128[] memory paid = new uint128[](end - start);
        for (uint256 i = start; i < end; ++i) {
            who[i - start] = _bidder(i);
            paid[i - start] = i < WINNERS ? LOCK / 2 : 0;
            refunded[i - start] = LOCK - paid[i - start];
        }
        return BridgeMsgCodec.encodeRefundInstructions(DAY, index, CHUNKS, who, refunded, paid);
    }

    function _deliverChunk(uint16 index) internal returns (uint256 spent) {
        bytes memory packet = _chunk(index);
        uint256 before = gasleft();
        _deliver(OUTBE_CHAIN_ID, originPeer, address(router), packet);
        spent = before - gasleft();
    }
}

contract RefundDayFirstChunkGasTest is RefundDayGasBase {
    function setUp() public {
        _setUpDay(0);
    }

    function test_TheFirstChunk() public {
        emit log_named_uint("day_chunk_first_40w_24l", _deliverChunk(0));
    }
}

contract RefundDayMiddleChunkGasTest is RefundDayGasBase {
    function setUp() public {
        _setUpDay(1);
    }

    function test_AMiddleChunk() public {
        emit log_named_uint("day_chunk_middle_64l", _deliverChunk(1));
    }
}

contract RefundDayClosingChunkGasTest is RefundDayGasBase {
    function setUp() public {
        _setUpDay(3);
    }

    function test_TheChunkThatClosesTheDay() public {
        uint256 spent = _deliverChunk(3);
        emit log_named_uint("day_chunk_closing_8l", spent);
        (, bool finalized,) = escrow.getAuctionStatus(DAY);
        assertTrue(finalized, "the closing chunk finalizes the day");
    }
}
