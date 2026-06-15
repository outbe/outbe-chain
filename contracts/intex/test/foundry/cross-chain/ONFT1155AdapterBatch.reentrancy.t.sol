// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {
    IONFT1155AdapterBatch,
    BatchSendParam,
    MultiRecipientSendParam
} from "@contracts/shared/interfaces/IONFT1155AdapterBatch.sol";
import {IERC1155Receiver} from "@openzeppelin/contracts/token/ERC1155/IERC1155Receiver.sol";
import {MessagingFee} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";
import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";

/// @dev Hostile ERC1155 receiver that, during the `onERC1155Received` callback fired
///      mid-`_lzReceive` (via `token.credit` → `_mint`), re-enters the adapter's
///      `multiSend` entrypoint. With both `_lzReceive` and `multiSend` carrying
///      OZ `nonReentrant`, the inner call reverts with `ReentrancyGuardReentrantCall`
///      at the modifier check (before the empty-batch validation), and we capture
///      the selector. Without the guards, the inner call would revert with
///      `EmptyBatch` instead — distinguishing the two cases.
contract ReentrantBatchProbe is IERC1155Receiver {
    address public immutable adapter;
    bool public attempted;
    bytes4 public observedSelector;

    constructor(address adapter_) {
        adapter = adapter_;
    }

    function onERC1155Received(address, address, uint256, uint256, bytes calldata) external returns (bytes4) {
        attempted = true;

        MultiRecipientSendParam memory param = MultiRecipientSendParam({
            dstEid: 0,
            recipients: new bytes32[](0),
            tokenIds: new uint256[](0),
            amounts: new uint256[](0),
            extraOptions: ""
        });
        MessagingFee memory fee = MessagingFee({nativeFee: 0, lzTokenFee: 0});

        try IONFT1155AdapterBatch(adapter).multiSend{value: 0}(param, fee, address(this)) {
        // unexpected: re-entrant call should always revert (either with the guard or with EmptyBatch)
        }
        catch (bytes memory err) {
            if (err.length >= 4) {
                bytes32 word;
                assembly {
                    word := mload(add(err, 0x20))
                }
                observedSelector = bytes4(word);
            }
        }

        return IERC1155Receiver.onERC1155Received.selector;
    }

    function onERC1155BatchReceived(address, address, uint256[] calldata, uint256[] calldata, bytes calldata)
        external
        pure
        returns (bytes4)
    {
        return IERC1155Receiver.onERC1155BatchReceived.selector;
    }

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == type(IERC1155Receiver).interfaceId;
    }
}

/// @title ONFT1155AdapterBatchReentrancyTest
/// @notice Behavioral test that `_lzReceive` and `multiSend` are mutually `nonReentrant`-guarded.
/// @dev Source chain (A) caller initiates a single-recipient batch transfer to the hostile probe
///      on the destination chain (B). On B, `_lzReceive` → `_handleBatchReceive` → `token.credit`
///      → `_mint` invokes the probe's `onERC1155Received`, which attempts to re-enter
///      `adapterB.multiSend`. Expected: the inner call reverts with
///      `ReentrancyGuardReentrantCall` — proving the guard is held by `_lzReceive` AND that
///      `multiSend` carries the modifier.
contract ONFT1155AdapterBatchReentrancyTest is TestHelperOz5 {
    using OptionsBuilder for bytes;

    uint32 private aEid = 1;
    uint32 private bEid = 2;

    IntexNFT1155 private tokenA;
    IntexNFT1155 private tokenB;
    ONFT1155AdapterBatch private adapterA;
    ONFT1155AdapterBatch private adapterB;

    address private user = address(0x1);
    uint32 private constant SERIES_ID = 20260401;
    uint256 private constant TOKEN_ID = uint256(SERIES_ID);
    uint256 private constant AMOUNT = 100;
    uint32 private constant ISSUED_INTEX_COUNT = 10_000;

    function setUp() public virtual override {
        vm.deal(user, 1000 ether);

        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        tokenA = DeployProxy.intexNFT1155(address(this), address(this));
        tokenB = DeployProxy.intexNFT1155(address(this), address(this));

        adapterA = DeployProxy.onftAdapterBatch(address(tokenA), address(endpoints[aEid]), address(this));
        adapterB = DeployProxy.onftAdapterBatch(address(tokenB), address(endpoints[bEid]), address(this));

        tokenA.grantRole(tokenA.RELAYER_ROLE(), address(adapterA));
        tokenB.grantRole(tokenB.RELAYER_ROLE(), address(adapterB));

        address[] memory oapps = new address[](2);
        oapps[0] = address(adapterA);
        oapps[1] = address(adapterB);
        this.wireOApps(oapps);

        tokenA.createSeries(SERIES_ID, ISSUED_INTEX_COUNT, 0);
        tokenB.createSeries(SERIES_ID, ISSUED_INTEX_COUNT, 0);

        tokenA.markQualified(SERIES_ID);
        tokenB.markQualified(SERIES_ID);

        tokenA.mint(user, AMOUNT, SERIES_ID);
    }

    function test_lzReceive_blocks_reentry_to_multiSend() public {
        ReentrantBatchProbe probe = new ReentrantBatchProbe(address(adapterB));

        uint256[] memory ids = new uint256[](1);
        ids[0] = TOKEN_ID;
        uint256[] memory amts = new uint256[](1);
        amts[0] = AMOUNT;
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(400000, 0);

        BatchSendParam memory sendParam = BatchSendParam({
            dstEid: bEid, to: addressToBytes32(address(probe)), tokenIds: ids, amounts: amts, extraOptions: options
        });

        MessagingFee memory fee = adapterA.quoteBatchSend(sendParam, false);

        vm.prank(user);
        adapterA.batchSend{value: fee.nativeFee}(sendParam, fee, user);

        verifyPackets(bEid, addressToBytes32(address(adapterB)));

        assertTrue(probe.attempted(), "probe callback never ran");
        assertEq(
            probe.observedSelector(),
            bytes4(keccak256("ReentrancyGuardReentrantCall()")),
            "_lzReceive / multiSend reentrancy guard did not fire"
        );
    }
}
