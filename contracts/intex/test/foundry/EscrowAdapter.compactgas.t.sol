// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {CrossChainTest} from "./cross-chain/../helpers/CrossChainTest.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

/// @dev `IntexGas.refund` is cut from a measurement taken against `MockTheCompact`. These pin what the
///      escrow's finalization actually costs against the canonical Compact, so the budget is sized on
///      the custody the target chain really uses rather than on the stand-in.
abstract contract EscrowFinalizeGasBase is Test {
    uint32 internal constant WORLDWIDE_DAY = 20260501;
    uint128 internal constant LOCK = 1000e18;

    EscrowAdapter internal escrow;
    IERC20 internal token;
    address internal admin = address(this);

    function _bidders(uint256 n) internal pure returns (address[] memory who) {
        who = new address[](n);
        for (uint256 i = 0; i < n; ++i) {
            who[i] = address(uint160(0x5000 + i));
        }
    }

    /// @dev Locks `n` bidders, then measures the all-refund finalization of the whole chunk.
    function _measure(uint256 n) internal returns (uint256 spent) {
        address[] memory who = _bidders(n);
        for (uint256 i = 0; i < n; ++i) {
            deal(address(token), who[i], LOCK);
            vm.prank(who[i]);
            token.approve(address(escrow), type(uint256).max);
            escrow.lockFunds(WORLDWIDE_DAY, who[i], LOCK);
        }

        // ResetPeriod.OneMinute: forced withdrawal only clears once the period has elapsed.
        vm.warp(block.timestamp + 5 minutes);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](n);
        for (uint256 i = 0; i < n; ++i) {
            instructions[i] =
                IEscrowAdapter.FinalizationInstruction({bidder: who[i], refundedAmount: LOCK, paidAmount: 0});
        }

        uint256 before = gasleft();
        escrow.finalizeAuction(WORLDWIDE_DAY, bytes32(uint256(1)), instructions, true);
        spent = before - gasleft();

        for (uint256 i = 0; i < n; ++i) {
            assertEq(token.balanceOf(who[i]), LOCK, "every bidder must be refunded in full");
        }
    }
}

/// @dev The canonical Compact on Ethereum mainnet, reached over a fork.
contract EscrowFinalizeGasRealCompactTest is EscrowFinalizeGasBase {
    address internal constant REAL_COMPACT = 0x00000000000000171ede64904551eeDF3C6C9788;
    address internal constant REAL_TOKEN = 0x5Dd5245b8D00341Bb6aaA222107999d788faaF61;

    function setUp() public {
        // Opt-in: without an archive-free mainnet endpoint the fork cannot be taken, and CI stays offline.
        string memory rpc = vm.envOr("ETH_MAINNET_RPC_URL", string(""));
        if (bytes(rpc).length == 0) {
            vm.skip(true);
            return;
        }
        vm.createSelectFork(rpc);

        escrow = DeployProxy.escrowAdapter(admin, admin);
        escrow.wire(admin, REAL_COMPACT, REAL_TOKEN);
        escrow.grantRole(escrow.AUCTION_ROLE(), admin);
        token = IERC20(REAL_TOKEN);
    }

    function test_FinalizeAgainstTheRealCompact() public {
        emit log_named_uint("refund_64_real_compact", _measure(64));
    }

    function test_FinalizeAgainstTheRealCompactTwoBidders() public {
        emit log_named_uint("refund_2_real_compact", _measure(2));
    }
}

/// @dev The same finalization against the stand-in the budgets were cut from.
contract EscrowFinalizeGasMockCompactTest is EscrowFinalizeGasBase {
    function setUp() public {
        MockTheCompact compact = new MockTheCompact();
        MockWCOEN payment = new MockWCOEN();
        compact.setResetPeriodSeconds(0);

        escrow = DeployProxy.escrowAdapter(admin, admin);
        escrow.wire(admin, address(compact), address(payment));
        escrow.grantRole(escrow.AUCTION_ROLE(), admin);
        token = IERC20(address(payment));
    }

    function test_FinalizeAgainstTheMock() public {
        emit log_named_uint("refund_64_mock_compact", _measure(64));
    }

    function test_FinalizeAgainstTheMockTwoBidders() public {
        emit log_named_uint("refund_2_mock_compact", _measure(2));
    }
}

/// @dev The escrow-level measurements above isolate `finalizeAuction`. `IntexGas.refund` has to cover the
///      whole inbound REFUND delivery, so this pins that end to end against the canonical Compact: the
///      budget is cut from this number rather than from a stand-in plus an estimated premium.
contract RefundMessageGasRealCompactTest is CrossChainTest {
    address internal constant REAL_COMPACT = 0x00000000000000171ede64904551eeDF3C6C9788;
    address internal constant REAL_TOKEN = 0x5Dd5245b8D00341Bb6aaA222107999d788faaF61;

    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant WORLDWIDE_DAY = 20260501;
    uint128 internal constant LOCK = 1000e18;

    TargetRouter internal router;
    EscrowAdapter internal escrow;
    IERC20 internal token;
    address internal admin = address(this);
    address internal originPeer = address(0x0B1);

    function setUp() public {
        string memory rpc = vm.envOr("ETH_MAINNET_RPC_URL", string(""));
        if (bytes(rpc).length == 0) {
            vm.skip(true);
            return;
        }
        vm.createSelectFork(rpc);

        _setUpBridge();
        escrow = DeployProxy.escrowAdapter(admin, admin);
        escrow.wire(admin, REAL_COMPACT, REAL_TOKEN);
        escrow.grantRole(escrow.AUCTION_ROLE(), admin);

        router = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        router.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, originPeer));
        router.wire(makeAddr("auction"), makeAddr("intex"), address(escrow));
        escrow.grantRole(escrow.RELAYER_ROLE(), address(router));

        token = IERC20(REAL_TOKEN);
    }

    function test_TheRefundQuoteCoversAWidestChunkAgainstTheRealCompact() public {
        uint256 bidders = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN;
        address[] memory who = new address[](bidders);
        uint128[] memory refunded = new uint128[](bidders);
        uint128[] memory paid = new uint128[](bidders);
        for (uint256 i = 0; i < bidders; ++i) {
            who[i] = address(uint160(0x5000 + i));
            deal(address(token), who[i], LOCK);
            vm.prank(who[i]);
            token.approve(address(escrow), type(uint256).max);
            escrow.lockFunds(WORLDWIDE_DAY, who[i], LOCK);
            refunded[i] = LOCK;
            paid[i] = 0;
        }

        // ResetPeriod.OneMinute: the forced withdrawal the escrow uses only clears once it has elapsed.
        vm.warp(block.timestamp + 5 minutes);

        bytes memory packet = BridgeMsgCodec.encodeRefundInstructions(WORLDWIDE_DAY, 0, 1, who, refunded, paid);
        uint256 before = gasleft();
        _deliver(OUTBE_CHAIN_ID, originPeer, address(router), packet);
        uint256 spent = before - gasleft();

        for (uint256 i = 0; i < bidders; ++i) {
            assertEq(token.balanceOf(who[i]), LOCK, "every bidder refunded in full");
        }
        emit log_named_uint("refund_message_64_real_compact", spent);
        assertLt(spent, IntexGas.refund(bidders), "the widest refund chunk must fit its quote");
    }
}
