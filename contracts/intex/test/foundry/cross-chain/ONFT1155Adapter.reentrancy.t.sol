// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {SendParam} from "@contracts/shared/interfaces/IONFT1155Adapter.sol";
import {IERC1155Receiver} from "@openzeppelin/contracts/token/ERC1155/IERC1155Receiver.sol";
import {MessagingFee} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";
import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {Vm} from "forge-std/Vm.sol";

/// @dev ERC1155 receiver that snapshots the adapter's OZ `ReentrancyGuard` storage
///      slot during the `onERC1155Received` callback. The slot reads `ENTERED == 2`
///      iff a `nonReentrant`-guarded function is active on `adapter` at callback time —
///      which only holds when `_lzReceive` carries the `nonReentrant` modifier.
contract ReentrancyGuardProbe is IERC1155Receiver {
    // keccak256(abi.encode(uint256(keccak256("openzeppelin.storage.ReentrancyGuard")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 internal constant REENTRANCY_GUARD_STORAGE =
        0x9b779b17422d0df92223018b32b4d1fa46e071723d6817e2486d003becc55f00;
    Vm internal constant VM = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    address public immutable adapter;
    uint256 public observedGuardSlot;
    bool public observed;

    constructor(address adapter_) {
        adapter = adapter_;
    }

    function onERC1155Received(address, address, uint256, uint256, bytes calldata) external returns (bytes4) {
        observedGuardSlot = uint256(VM.load(adapter, REENTRANCY_GUARD_STORAGE));
        observed = true;
        return IERC1155Receiver.onERC1155Received.selector;
    }

    function onERC1155BatchReceived(address, address, uint256[] calldata, uint256[] calldata, bytes calldata)
        external
        returns (bytes4)
    {
        observedGuardSlot = uint256(VM.load(adapter, REENTRANCY_GUARD_STORAGE));
        observed = true;
        return IERC1155Receiver.onERC1155BatchReceived.selector;
    }

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == type(IERC1155Receiver).interfaceId;
    }
}

/// @title ONFT1155AdapterReentrancyTest
/// @notice Behavioral test that `ONFT1155Adapter._lzReceive` runs under OZ `nonReentrant`.
/// @dev `OAppReceiver.lzReceive` is `onlyEndpoint`-gated, so a hostile receiver re-entering
///      the public entrypoint always hits `OnlyEndpoint` before the reentrancy guard fires.
///      We can't behaviorally observe `ReentrancyGuardReentrantCall` without bypassing the
///      role gate. Instead we probe the guard's storage slot mid-callback: it equals
///      `ENTERED (2)` iff `_lzReceive` carries the modifier — that is the visible signature
///      of the defense-in-depth layer and what this test pins.
contract ONFT1155AdapterReentrancyTest is TestHelperOz5 {
    using OptionsBuilder for bytes;

    uint256 internal constant ENTERED = 2;

    uint32 private aEid = 1;
    uint32 private bEid = 2;

    IntexNFT1155 private tokenA;
    IntexNFT1155 private tokenB;
    ONFT1155Adapter private adapterA;
    ONFT1155Adapter private adapterB;

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

        adapterA = DeployProxy.onftAdapter(address(tokenA), address(endpoints[aEid]), address(this), bEid);
        adapterB = DeployProxy.onftAdapter(address(tokenB), address(endpoints[bEid]), address(this), aEid);

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

    function test_lzReceive_runsUnderNonReentrant() public {
        ReentrancyGuardProbe probe = new ReentrancyGuardProbe(address(adapterB));

        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(400000, 0);
        SendParam memory sendParam = SendParam({
            dstEid: bEid,
            to: addressToBytes32(address(probe)),
            tokenId: TOKEN_ID,
            amount: AMOUNT,
            extraOptions: options,
            composeMsg: ""
        });

        MessagingFee memory fee = adapterA.quoteSend(sendParam, false);

        vm.prank(user);
        adapterA.send{value: fee.nativeFee}(sendParam, fee, user);

        verifyPackets(bEid, addressToBytes32(address(adapterB)));

        assertTrue(probe.observed(), "probe callback never ran");
        assertEq(probe.observedGuardSlot(), ENTERED, "_lzReceive missing nonReentrant modifier");
    }
}
