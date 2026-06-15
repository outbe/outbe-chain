// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {EscrowAdapter} from "@contracts/bnb/EscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {IEscrowAdapter} from "@contracts/bnb/interfaces/IEscrowAdapter.sol";
import {IVaultProvider} from "@contracts/vendor/outbe-vault/interfaces/IVaultProvider.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockERC20} from "@test-mocks/MockERC20.sol";
import {MockSettlementVault} from "@test-mocks/MockSettlementVault.sol";
import {MockVaultProvider} from "@test-mocks/MockVaultProvider.sol";

/// @dev Self-call shim guards on EscrowAdapter. `processFinalizationOne` wraps the per-bidder
///      `_processFinalizationInstruction` for `finalizeAuction`'s try/catch; `settleVaultOwedSelf`
///      wraps `_settleVaultOwed` for `claimRefund`'s vault-deposit isolation. Both must reject
///      any external caller so the wrapped logic only runs under the outer entry-point's role
///      gate and reentrancy guard.
contract EscrowAdapterNotSelfTest is Test {
    EscrowAdapter internal escrow;

    address internal admin = address(1);
    address internal bridger = address(2);
    address internal auction = address(3);
    address internal bidder = address(0xB1);

    uint32 internal constant SERIES_ID = 1;
    bytes32 internal constant GUID = bytes32(uint256(0xCAFE));

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, bridger);
        MockTheCompact compact = new MockTheCompact();
        MockERC20 paymentToken = new MockERC20("USD Coin", "USDC", 6);
        MockSettlementVault vault = new MockSettlementVault(address(paymentToken), "Mock Vault USDC", "mvUSDC", 6);
        MockVaultProvider provider = new MockVaultProvider();
        provider.addVault(vault);
        provider.addLiquiditySource(address(escrow), IVaultProvider.LiquiditySource.IntexBidPrice);
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(provider), address(paymentToken));
    }

    function test_processFinalizationOne_externalCallerRevertsNotSelf() public {
        IEscrowAdapter.FinalizationInstruction memory inst =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder, refundedAmount: 1, paidAmount: 0});
        vm.expectRevert(IEscrowAdapter.NotSelf.selector);
        escrow.processFinalizationOne(SERIES_ID, GUID, inst);
    }

    function test_settleVaultOwedSelf_externalCallerRevertsNotSelf() public {
        vm.expectRevert(IEscrowAdapter.NotSelf.selector);
        escrow.settleVaultOwedSelf(SERIES_ID, bidder);
    }
}
