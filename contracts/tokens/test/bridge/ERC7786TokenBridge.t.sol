// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {ERC7786TokenBridge} from "../../src/ERC7786TokenBridge.sol";
import {USDT} from "../../src/canonical/USDT.sol";
import {BridgeableERC20Stable} from "../../src/synthetic/BridgeableERC20Stable.sol";
import {WCOEN as NativeWCOEN} from "../../src/canonical/WCOEN.sol";
import {BridgeableERC20 as SyntheticWCOEN} from "../../src/synthetic/BridgeableERC20.sol";
import {MockERC7786Bridge} from "../mocks/MockERC7786Bridge.sol";
import {MockStablecoin} from "../mocks/MockStablecoin.sol";

/// @dev Burns 1% of every transfer between two holders.
contract FeeOnTransferToken is ERC20("Fee", "FEE") {
    function decimals() public pure override returns (uint8) {
        return 6;
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function _update(address from, address to, uint256 value) internal override {
        if (from == address(0) || to == address(0)) return super._update(from, to, value);
        uint256 fee = value / 100;
        super._update(from, address(0), fee);
        super._update(from, to, value - fee);
    }
}

contract ERC7786TokenBridgeTest is Test {
    ERC7786TokenBridge.TokenBridgeMode internal constant LOCK = ERC7786TokenBridge.TokenBridgeMode.LockUnlock;
    ERC7786TokenBridge.TokenBridgeMode internal constant BURN = ERC7786TokenBridge.TokenBridgeMode.BurnMint;

    uint32 internal constant BNB = 97;
    uint32 internal constant OUTBE = 54322345;

    address internal sourceAddr = makeAddr("sourceAddr");
    address internal targetAddr = makeAddr("targetAddr");
    address internal intruder = makeAddr("intruder");

    MockERC7786Bridge internal bnbGateway;
    MockERC7786Bridge internal outbeGateway;

    USDT internal usdt;
    BridgeableERC20Stable internal usdt0;
    ERC7786TokenBridge internal bnbUsdtBridge;
    ERC7786TokenBridge internal outbeUsdt0Bridge;

    NativeWCOEN internal outbeWcoen;
    SyntheticWCOEN internal bnbWcoen;
    ERC7786TokenBridge internal outbeWcoenBridge;
    ERC7786TokenBridge internal bnbWcoenBridge;

    MockStablecoin internal stable;
    ERC7786TokenBridge internal bnbStableBridge;
    ERC7786TokenBridge internal outbeStableBridge;

    function setUp() public {
        bnbGateway = new MockERC7786Bridge(BNB);
        outbeGateway = new MockERC7786Bridge(OUTBE);
        bnbGateway.setRemoteBridge(OUTBE, outbeGateway);
        outbeGateway.setRemoteBridge(BNB, bnbGateway);

        _setUpUsdtRoute();
        _setUpWcoenRoute();
        _setUpStablecoinRoute();
    }

    function test_USDT_BNBToOutbe_LockAndMint() public {
        uint256 amount = 100e6;
        usdt.mint(sourceAddr, amount);

        vm.startPrank(sourceAddr);
        usdt.approve(address(bnbUsdtBridge), amount);
        bnbUsdtBridge.send(OUTBE, targetAddr, amount);
        vm.stopPrank();

        assertEq(usdt.balanceOf(sourceAddr), 0);
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), amount);
        assertEq(usdt0.balanceOf(targetAddr), amount);
    }

    function test_USDT_OutbeToBNB_BurnAndUnlock() public {
        uint256 amount = 100e6;
        usdt.mint(sourceAddr, amount);

        vm.startPrank(sourceAddr);
        usdt.approve(address(bnbUsdtBridge), amount);
        bnbUsdtBridge.send(OUTBE, targetAddr, amount);
        vm.stopPrank();

        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), 100e6);

        vm.prank(targetAddr);
        outbeUsdt0Bridge.send(BNB, sourceAddr, 40e6);

        assertEq(usdt0.balanceOf(targetAddr), 60e6);
        assertEq(usdt.balanceOf(sourceAddr), 40e6);
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), 60e6);
    }

    function test_WCOEN_OutbeToBNB_LockAndMint() public {
        uint256 amount = 2e6;
        vm.deal(sourceAddr, amount);

        vm.startPrank(sourceAddr);
        outbeWcoen.deposit{value: amount}();
        outbeWcoen.approve(address(outbeWcoenBridge), amount);
        outbeWcoenBridge.send(BNB, targetAddr, amount);
        vm.stopPrank();

        assertEq(outbeWcoen.balanceOf(sourceAddr), 0);
        assertEq(outbeWcoen.balanceOf(address(outbeWcoenBridge)), amount);
        assertEq(bnbWcoen.balanceOf(targetAddr), amount);
    }

    function test_WCOEN_BNBToOutbe_BurnAndUnlock() public {
        uint256 amount = 2e6;
        vm.deal(sourceAddr, amount);

        vm.startPrank(sourceAddr);
        outbeWcoen.deposit{value: amount}();
        outbeWcoen.approve(address(outbeWcoenBridge), amount);
        outbeWcoenBridge.send(BNB, targetAddr, amount);
        vm.stopPrank();

        vm.prank(targetAddr);
        bnbWcoenBridge.send(OUTBE, sourceAddr, 1e6);

        assertEq(bnbWcoen.balanceOf(targetAddr), 1e6);
        assertEq(outbeWcoen.balanceOf(sourceAddr), 1e6);
        assertEq(outbeWcoen.balanceOf(address(outbeWcoenBridge)), 1e6);
    }

    function test_Quote_DelegatesToBridge() public {
        outbeGateway.setFeeQuote(123);
        vm.prank(targetAddr);
        assertEq(outbeUsdt0Bridge.quoteSend(BNB, sourceAddr, 1, "", 0), 123);
    }

    function test_RevertWhen_SendToZeroRecipient() public {
        usdt.mint(sourceAddr, 1);

        vm.startPrank(sourceAddr);
        usdt.approve(address(bnbUsdtBridge), 1);
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.InvalidRecipient.selector, address(0)));
        bnbUsdtBridge.send(OUTBE, address(0), 1);
        vm.stopPrank();

        assertEq(usdt.balanceOf(sourceAddr), 1);
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), 0);
    }

    function test_RevertWhen_ReceiveFromNonBridge() public {
        vm.prank(intruder);
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.UnauthorizedBridge.selector, intruder));
        outbeUsdt0Bridge.receiveMessage(bytes32(0), _interop(BNB, address(bnbUsdtBridge)), "");
    }

    function test_RevertWhen_ReceiveFromWrongRemoteBridge() public {
        bytes memory wrongSender = _interop(BNB, intruder);

        vm.prank(address(outbeGateway));
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.UnauthorizedRemoteBridge.selector, wrongSender));
        outbeUsdt0Bridge.receiveMessage(bytes32(0), wrongSender, "");
    }

    function test_FeeOnTransferLock_CreditsReceivedAmount() public {
        FeeOnTransferToken feeToken = new FeeOnTransferToken();
        ERC7786TokenBridge feeBridge = _bridge(address(feeToken), bnbGateway, LOCK, OUTBE, _nextAddress(2));
        SyntheticWCOEN feeSynthetic = new SyntheticWCOEN("Fee", "FEE", 6, _nextAddress(1));
        _bridge(address(feeSynthetic), outbeGateway, BURN, BNB, address(feeBridge));
        feeToken.mint(sourceAddr, 100e6);

        vm.startPrank(sourceAddr);
        feeToken.approve(address(feeBridge), 100e6);
        feeBridge.send(OUTBE, targetAddr, 100e6);
        vm.stopPrank();

        assertEq(feeToken.balanceOf(address(feeBridge)), 99e6);
        assertEq(feeSynthetic.balanceOf(targetAddr), 99e6);
    }

    function test_AddPeer_ConnectsNewDomain() public {
        address peer = makeAddr("peer");
        bnbUsdtBridge.addPeer(1, peer);
        assertEq(bnbUsdtBridge.remoteBridges(1), peer);
    }

    function test_RevertWhen_AddPeerChangesExistingPeer() public {
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.InvalidPeer.selector, OUTBE));
        bnbUsdtBridge.addPeer(OUTBE, intruder);
    }

    function test_RevertWhen_AddPeerByNonOwner() public {
        vm.prank(intruder);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, intruder));
        bnbUsdtBridge.addPeer(1, intruder);
    }

    function test_RevertWhen_LockSideHasNoAllowance() public {
        usdt.mint(sourceAddr, 1);

        vm.prank(sourceAddr);
        vm.expectRevert();
        bnbUsdtBridge.send(OUTBE, targetAddr, 1);
    }

    function test_Stablecoin_BNBToOutbe_LockAndMint() public {
        uint256 amount = 100e6;
        usdt.mint(sourceAddr, amount);

        vm.startPrank(sourceAddr);
        usdt.approve(address(bnbStableBridge), amount);
        bnbStableBridge.send(OUTBE, targetAddr, amount);
        vm.stopPrank();

        assertEq(usdt.balanceOf(address(bnbStableBridge)), amount, "locked on the canonical side");
        assertEq(stable.balanceOf(targetAddr), amount, "minted on the issuer side");
    }

    function test_Stablecoin_OutbeToBNB_BurnAndUnlock() public {
        uint256 amount = 100e6;
        usdt.mint(address(bnbStableBridge), amount);
        stable.grantIssuer(address(this));
        stable.mint(sourceAddr, amount);

        vm.startPrank(sourceAddr);
        stable.approve(address(outbeStableBridge), amount);
        outbeStableBridge.send(BNB, targetAddr, amount);
        vm.stopPrank();

        assertEq(stable.balanceOf(sourceAddr), 0, "burned on the issuer side");
        assertEq(stable.totalSupply(), 0, "supply follows the burn");
        assertEq(usdt.balanceOf(targetAddr), amount, "unlocked on the canonical side");
    }

    /// @dev `burnFrom` spends an allowance, so an unapproved sender must not get as far as the
    ///      message: the tokens would be minted on the far side against nothing burned here.
    function test_RevertWhen_IssuerBurnHasNoAllowance() public {
        stable.grantIssuer(address(this));
        stable.mint(sourceAddr, 1);

        vm.prank(sourceAddr);
        vm.expectRevert();
        outbeStableBridge.send(BNB, targetAddr, 1);

        assertEq(stable.balanceOf(sourceAddr), 1, "nothing burned");
    }

    function test_RevertWhen_BridgeLacksIssuerRole() public {
        stable.revokeIssuer(address(outbeStableBridge));
        uint8 decimals = outbeStableBridge.tokenDecimals();

        vm.prank(address(outbeGateway));
        vm.expectRevert(abi.encodeWithSelector(MockStablecoin.NotIssuer.selector, address(outbeStableBridge)));
        outbeStableBridge.receiveMessage(
            bytes32(0),
            _interop(BNB, address(bnbStableBridge)),
            abi.encode(abi.encodePacked(sourceAddr), targetAddr, uint256(1), decimals, bytes(""))
        );
    }

    function test_RevertWhen_SourceTokenHasOtherDecimals() public {
        uint8 local = outbeStableBridge.tokenDecimals();

        vm.prank(address(outbeGateway));
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.DecimalsMismatch.selector, local + 12, local));
        outbeStableBridge.receiveMessage(
            bytes32(0),
            _interop(BNB, address(bnbStableBridge)),
            abi.encode(abi.encodePacked(sourceAddr), targetAddr, uint256(1), local + 12, bytes(""))
        );
    }

    function _setUpStablecoinRoute() internal {
        stable = new MockStablecoin("USD Tether", "USDT", address(this));

        bnbStableBridge = _bridge(address(usdt), bnbGateway, LOCK, OUTBE, _nextAddress(1));
        outbeStableBridge = _bridge(
            address(stable),
            outbeGateway,
            ERC7786TokenBridge.TokenBridgeMode.IssuerBurnMint,
            BNB,
            address(bnbStableBridge)
        );

        // The issuer side is authorized by role, not by a token->bridge link.
        stable.grantIssuer(address(outbeStableBridge));
    }

    function _setUpUsdtRoute() internal {
        usdt = new USDT();
        bnbUsdtBridge = _bridge(address(usdt), bnbGateway, LOCK, OUTBE, _nextAddress(2));
        usdt0 = new BridgeableERC20Stable("USDT0", "USDT0", 6, 840, _nextAddress(1));
        outbeUsdt0Bridge = _bridge(address(usdt0), outbeGateway, BURN, BNB, address(bnbUsdtBridge));
    }

    function _setUpWcoenRoute() internal {
        outbeWcoen = new NativeWCOEN();
        outbeWcoenBridge = _bridge(address(outbeWcoen), outbeGateway, LOCK, BNB, _nextAddress(2));
        bnbWcoen = new SyntheticWCOEN("Wrapped COEN", "WCOEN", 18, _nextAddress(1));
        bnbWcoenBridge = _bridge(address(bnbWcoen), bnbGateway, BURN, OUTBE, address(outbeWcoenBridge));
    }

    /// @dev The address of the contract this test deploys after `skip` other deployments.
    function _nextAddress(uint256 skip) internal view returns (address) {
        return vm.computeCreateAddress(address(this), vm.getNonce(address(this)) + skip);
    }

    /// @dev A bridge owned by this test, with one peer.
    function _bridge(
        address token,
        MockERC7786Bridge gateway,
        ERC7786TokenBridge.TokenBridgeMode mode,
        uint32 peerDomain,
        address peer
    ) internal returns (ERC7786TokenBridge) {
        ERC7786TokenBridge.Peer[] memory peers = new ERC7786TokenBridge.Peer[](1);
        peers[0] = ERC7786TokenBridge.Peer(peerDomain, peer);
        return new ERC7786TokenBridge(token, address(gateway), mode, address(this), peers);
    }

    function _interop(uint256 chainId, address addr) internal pure returns (bytes memory) {
        return InteroperableAddress.formatEvmV1(chainId, addr);
    }
}
