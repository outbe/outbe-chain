// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "./helpers/CrossChainTest.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";
import {IntexUnits} from "@contracts/shared/libs/IntexUnits.sol";
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
    bytes32 internal constant ESCROW_STORAGE_SLOT = 0x9dc6707131c30ec20e38ebcfbc4641faad640e3439439d400ea9dd2fe8f83a00;
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant DAY = 20260501;
    uint128 internal constant BASIS = 1000e6;
    uint16 internal constant QUANTITY = 10;
    uint32 internal constant BID_RATE = 900_000;
    uint32 internal constant CLEARING_RATE = 600_000;

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

    function _winners(uint256 count) internal pure returns (address[] memory who) {
        who = new address[](count);
        for (uint256 i = 0; i < count; ++i) {
            who[i] = _bidder(i);
        }
    }

    /// @dev `held` stays on each bidder's balance after locking, so a refund lands on a balance already in use.
    function _lock(uint256 from, uint256 count, uint32 bidRate, uint128 held) internal {
        uint128 amount = uint128(IntexUnits.escrowAmount(QUANTITY, BASIS, bidRate));
        for (uint256 i = from; i < from + count; ++i) {
            address who = _bidder(i);
            deal(address(token), who, uint256(amount) + held);
            vm.prank(who);
            token.approve(address(escrow), type(uint256).max);
            escrow.lockFunds(DAY, who, amount, bidRate, QUANTITY);
        }
    }

    function _elapseCompactResetPeriod() internal {
        vm.warp(block.timestamp + 5 minutes);
    }

    function _finalize(uint256 n) internal returns (uint256 spent) {
        address[] memory winners = _winners(n);
        uint256 before = gasleft();
        escrow.finalizeAuction(DAY, bytes32(uint256(1)), winners, 0, 0, CLEARING_RATE, BASIS, false);
        spent = before - gasleft();
    }
}

contract RefundFinalizeGasTest is RefundGasBase {
    function setUp() public {
        _setUpEscrow();
        _lock(0, 128, BID_RATE, 0);
        _elapseCompactResetPeriod();
    }

    function test_Finalize1Winner() public {
        emit log_named_uint("finalize_winners_1", _finalize(1));
    }

    function test_Finalize2Winners() public {
        emit log_named_uint("finalize_winners_2", _finalize(2));
    }

    function test_Finalize8Winners() public {
        emit log_named_uint("finalize_winners_8", _finalize(8));
    }

    function test_Finalize32Winners() public {
        emit log_named_uint("finalize_winners_32", _finalize(32));
    }

    function test_Finalize64Winners() public {
        emit log_named_uint("finalize_winners_64", _finalize(64));
    }

    function test_Finalize128Winners() public {
        emit log_named_uint("finalize_winners_128", _finalize(128));
    }
}

/// @dev Bids at the clearing rate pay their whole lock, so each lock is deleted instead of kept for a claim.
contract RefundFinalizeWholeLockGasTest is RefundGasBase {
    function setUp() public {
        _setUpEscrow();
        _lock(0, 128, CLEARING_RATE, 0);
        _elapseCompactResetPeriod();
    }

    function test_Finalize128WinnersPayingTheirWholeLock() public {
        emit log_named_uint("finalize_winners_128_whole_lock", _finalize(128));
    }
}

contract RefundClaimUnfinalizedGasTest is RefundGasBase {
    function setUp() public {
        _setUpEscrow();
        _lock(0, 1, BID_RATE, 0);
        vm.warp(block.timestamp + escrow.UNFINALIZED_REFUND_DELAY());
    }

    function test_ClaimOnADayThatNeverFinalized() public {
        uint256 before = gasleft();
        escrow.claimRefund(DAY, _bidder(0));
        emit log_named_uint("claim_unfinalized", before - gasleft());
    }
}

/// @dev Bidders 0 and 1 won, bidder 1 already holding the token; bidder 2 lost; bidder 3 carries a split recorded
///      before refunds became claims.
contract RefundClaimFinalizedGasTest is RefundGasBase {
    function setUp() public {
        _setUpEscrow();
        _lock(0, 1, BID_RATE, 0);
        _lock(1, 1, BID_RATE, 1e18);
        _lock(2, 2, BID_RATE, 0);
        _elapseCompactResetPeriod();

        bytes32 dayMap = keccak256(abi.encode(uint256(DAY), uint256(ESCROW_STORAGE_SLOT) + 5));
        bytes32 splitWord = bytes32(uint256(keccak256(abi.encode(_bidder(3), dayMap))) + 1);
        vm.store(address(escrow), splitWord, bytes32(uint256(1000e18) | (uint256(1) << 128)));

        escrow.finalizeAuction(DAY, bytes32(uint256(1)), _winners(2), 0, 0, CLEARING_RATE, BASIS, true);
    }

    function test_ClaimAsAWinner() public {
        uint256 before = gasleft();
        escrow.claimRefund(DAY, _bidder(0));
        emit log_named_uint("claim_finalized_winner", before - gasleft());
    }

    function test_ClaimAsAWinnerHoldingTheToken() public {
        uint256 before = gasleft();
        escrow.claimRefund(DAY, _bidder(1));
        emit log_named_uint("claim_finalized_winner_held", before - gasleft());
    }

    function test_ClaimAsALoser() public {
        uint256 before = gasleft();
        escrow.claimRefund(DAY, _bidder(2));
        emit log_named_uint("claim_finalized_loser", before - gasleft());
    }

    function test_ClaimARecordedSplit() public {
        uint256 before = gasleft();
        escrow.claimRefund(DAY, _bidder(3));
        emit log_named_uint("claim_finalized_split", before - gasleft());
    }
}

/// @notice A day of 200 bidders reaching a target chain in one chunk that carries its winners, closes the day and
///         routes its proceeds; the last winner is filled in part.
contract RefundDayGasTest is RefundGasBase {
    uint256 internal constant BIDDERS = 200;

    TargetRouter internal router;
    address internal originPeer = address(0x0B1);

    function setUp() public {
        _setUpBridge();
        _setUpEscrow();

        router = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        router.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, originPeer));
        router.wire(makeAddr("auction"), makeAddr("intex"), address(escrow));
        router.setProceedsRoute(address(new PullingTokenBridge(token)), makeAddr("originRouter"));
        escrow.setProceedsRecipient(address(router));
        escrow.grantRole(escrow.RELAYER_ROLE(), address(router));
        vm.deal(address(router), 1 ether);

        _lock(0, BIDDERS, BID_RATE, 0);
        _elapseCompactResetPeriod();
    }

    function _deliverChunk(uint16 winnerCount) internal returns (uint256 spent) {
        uint16 partialIndex = winnerCount == 0 ? 0 : winnerCount - 1;
        uint16 partialWon = winnerCount == 0 ? 0 : QUANTITY / 2;
        bytes memory packet = BridgeMsgCodec.encodeRefundInstructions(
            DAY, 0, 1, CLEARING_RATE, BASIS, _winners(winnerCount), partialIndex, partialWon
        );
        uint256 before = gasleft();
        _deliver(OUTBE_CHAIN_ID, originPeer, address(router), packet);
        spent = before - gasleft();

        (, bool finalized,) = escrow.getAuctionStatus(DAY);
        assertTrue(finalized, "the only chunk closes the day");
    }

    function test_ADayWith40Winners() public {
        emit log_named_uint("day_chunk_40w", _deliverChunk(40));
    }

    function test_AWidestChunkFitsItsQuote() public {
        uint256 spent = _deliverChunk(BridgeMsgCodec.MAX_REFUND_WINNERS);
        emit log_named_uint("day_chunk_128w", spent);
        assertLt(spent, IntexGas.refund(BridgeMsgCodec.MAX_REFUND_WINNERS), "the widest chunk must fit its quote");
    }

    function test_AChainWithoutWinners() public {
        emit log_named_uint("day_chunk_empty", _deliverChunk(0));
    }
}
