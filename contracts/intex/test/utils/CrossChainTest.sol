// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {MockERC7786Bridge} from "@test-mocks/MockERC7786Bridge.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";

/// @dev Base for cross-chain protocol tests. A single loopback {MockERC7786Bridge} acts as the hub.
///      Logical chainIds are explicit and delivery is manual (via {_deliver}). Thus a send never
///      auto-loops unless a test opts in. Replaces the ERC-7786 `TestHelperOz5` harness.
abstract contract CrossChainTest is Test {
    MockERC7786Bridge internal bridge;

    /// @dev IntexAuctionTargetRouters that {_deliver} feeds through `receiveFromOutbe` instead of the hub.
    mapping(address router => bool) private _viaOutbeIca;

    /// @dev Deploy the loopback bridge with manual delivery. Call from `setUp`.
    function _setUpBridge() internal {
        bridge = new MockERC7786Bridge();
        bridge.setAutoDeliver(false);
    }

    /// @dev ERC-7930 interoperable address for `a` on `chainId`.
    function _interop(uint32 chainId, address a) internal pure returns (bytes memory) {
        return InteroperableAddress.formatEvmV1(chainId, a);
    }

    /// @dev How many recorded logs carry `sig` as topic0. Requires a prior `vm.recordLogs()`.
    function _countTopic(bytes32 sig) internal returns (uint256 n) {
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] == sig) n++;
        }
    }

    /// @dev Route the {_deliver} calls to `router` through `receiveFromOutbe`, sent by `ica`, the
    ///      Interchain Account of Outbe the router was deployed with.
    function _wireOutbeIca(IntexAuctionTargetRouter router, address ica) internal {
        assertEq(router.outbeIca(), ica, "the router takes the messages of this account");
        _viaOutbeIca[address(router)] = true;
    }

    /// @dev Deliver `packet` to `recipient` as if `src` on `srcChainId` sent it. A IntexAuctionTargetRouter wired
    ///      with {_wireOutbeIca} takes it from `src` through `receiveFromOutbe`; any other recipient
    ///      takes it from the bridge.
    function _deliver(uint32 srcChainId, address src, address recipient, bytes memory packet) internal {
        if (_viaOutbeIca[recipient]) {
            vm.prank(src);
            IntexAuctionTargetRouter(payable(recipient)).receiveFromOutbe(packet);
            return;
        }
        bridge.deliverAs(_interop(srcChainId, src), _interop(uint32(block.chainid), recipient), packet);
    }
}
