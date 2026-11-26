// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test, Vm} from "forge-std/Test.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {INTEX_FACTORY} from "@precompiles/IIntexFactory.sol";
import {GEM_FACTORY} from "@precompiles/IGemFactory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {IIntexNFT1155Router} from "@contracts/shared/interfaces/IIntexNFT1155Router.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IntexCreate3} from "../../../script/IntexCreate3.sol";
import {IntexScript} from "../../../script/IntexScript.sol";
import {DeployAll} from "../../../script/DeployAll.s.sol";
import {DeployIntexAuctionTargetRouter} from "../../../script/5_DeployIntexAuctionTargetRouter.s.sol";
import {MockTheCompact} from "../../mocks/MockTheCompact.sol";
import {MockWCOEN} from "../../mocks/MockWCOEN.sol";
import {DeployAllHarness} from "./ScriptHarness.sol";

contract DeployAllTest is Test {
    uint256 internal constant PK = 0xA11CE;
    string internal constant SALT = "v-deploy-all";
    uint32 internal constant OUTBE_CHAIN_ID = 54_322_345;
    address internal constant HUB = address(0xB0B);
    address internal constant WCOEN_BRIDGE = address(0xC0E);
    address internal constant OUTBE_ICA = address(0x1CA);
    uint256 internal constant PEER = 97;

    DeployAllHarness internal script;
    Create3Factory internal factory;
    MockTheCompact internal compact;
    MockWCOEN internal wcoen;
    address internal deployer;

    function setUp() public {
        script = new DeployAllHarness();
        factory = new Create3Factory();
        compact = new MockTheCompact();
        wcoen = new MockWCOEN();
        deployer = vm.addr(PK);
        script.setEnv(address(factory), SALT, false, OUTBE_ICA);
    }

    function _config(bool outbe) internal view returns (DeployAll.Config memory c) {
        uint256[] memory peers = new uint256[](2);
        peers[0] = block.chainid;
        peers[1] = PEER;
        c = DeployAll.Config({
            outbe: outbe,
            compact: address(compact),
            wcoen: address(wcoen),
            peerChainIds: peers,
            transport: DeployIntexAuctionTargetRouter.Transport({
                bridge: HUB, outbeChainId: OUTBE_CHAIN_ID, wcoenBridge: WCOEN_BRIDGE, outbeIca: OUTBE_ICA
            })
        });
    }

    function test_DeployAll_PutsEachProxyAtItsPredictedAddressOwnedByTheDeployer() public {
        script.deployWith(PK, _config(false));

        IntexCreate3.Stack memory s = script.stack(deployer);
        assertEq(s.intexNFT1155, factory.predict(deployer, IntexCreate3.salt(IntexCreate3.INTEX_NFT1155, SALT)));
        address[5] memory proxies = IntexCreate3.proxies(s);
        for (uint256 i = 0; i < proxies.length; i++) {
            assertGt(proxies[i].code.length, 0, "deployed");
            assertEq(Ownable(proxies[i]).owner(), deployer, "the deployer owns it until the hand-over");
        }
    }

    function test_DeployAll_LinksEveryContractToThePredictedOthers() public {
        script.deployWith(PK, _config(false));
        IntexCreate3.Stack memory s = script.stack(deployer);

        IntexNFT1155 nft = IntexNFT1155(s.intexNFT1155);
        assertEq(nft.targetRouter(), s.targetRouter);
        assertEq(nft.intexNFT1155Router(), s.intexNFT1155Router);
        assertEq(nft.intexAuction(), s.intexAuction);
        assertEq(nft.intexFactory(), address(0), "nothing burns the settled units off Outbe");
        assertEq(nft.gemFactory(), address(0), "no Gem Factory off Outbe");
        assertEq(EscrowAdapter(payable(s.escrowAdapter)).targetRouter(), s.targetRouter);
        assertEq(address(IntexAuction(s.intexAuction).escrow()), s.escrowAdapter);
        assertEq(IntexAuctionTargetRouter(payable(s.targetRouter)).outbeIca(), OUTBE_ICA);
    }

    function test_DeployAll_OnOutbeTheFactoriesBurnTheUnits() public {
        script.deployWith(PK, _config(true));
        IntexNFT1155 nft = IntexNFT1155(script.stack(deployer).intexNFT1155);
        assertEq(nft.intexFactory(), INTEX_FACTORY);
        assertEq(nft.gemFactory(), GEM_FACTORY);
    }

    function test_DeployAll_PairsTheNftRouterWithEveryOtherPeerInInitialize() public {
        address nftRouter = script.stack(deployer).intexNFT1155Router;
        bytes memory peer = InteroperableAddress.formatEvmV1(PEER, nftRouter);
        vm.recordLogs();

        script.deployWith(PK, _config(false));

        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 found;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter != nftRouter || logs[i].topics[0] != IIntexNFT1155Router.RemoteRouterSet.selector) {
                continue;
            }
            found++;
            assertEq(logs[i].topics[1], bytes32(PEER), "indexed chain id");
            assertEq(abi.decode(logs[i].data, (bytes)), peer, "the router on the peer");
        }
        assertEq(found, 1, "one event per peer, none for this chain");
        assertEq(IntexNFT1155Router(payable(nftRouter)).remoteRouter(uint32(PEER)), peer);
        assertEq(IntexNFT1155Router(payable(nftRouter)).remoteRouter(uint32(block.chainid)).length, 0, "no self pair");
    }

    function test_DeployAll_SetsTheEscrowAssetInInitialize() public {
        address escrow = script.stack(deployer).escrowAdapter;
        vm.expectEmit(true, true, true, true, escrow);
        emit IEscrowAdapter.IntexAuctionAssetSet(address(compact), address(wcoen), 18);

        script.deployWith(PK, _config(false));

        assertEq(address(EscrowAdapter(payable(escrow)).compact()), address(compact));
        assertEq(address(EscrowAdapter(payable(escrow)).intexAuctionAsset()), address(wcoen));
    }

    function test_DeployAll_ReRunKeepsTheProxiesAndWritesNothing() public {
        script.deployWith(PK, _config(false));

        vm.startStateDiffRecording();
        script.deployWith(PK, _config(false));
        Vm.AccountAccess[] memory accesses = vm.stopAndReturnStateDiff();
        for (uint256 i = 0; i < accesses.length; i++) {
            assertFalse(accesses[i].kind == VmSafe.AccountAccessKind.Create, "a re-run deploys nothing");
            for (uint256 j = 0; j < accesses[i].storageAccesses.length; j++) {
                assertFalse(accesses[i].storageAccesses[j].isWrite, "a re-run writes nothing");
            }
        }
    }

    function test_CheckLinks_RevertsWhenALinkDiffers() public {
        script.deployWith(PK, _config(false));
        DeployAll.Config memory c = _config(false);
        c.transport.outbeIca = address(0xBAD);

        IntexCreate3.Stack memory s = script.stack(deployer);
        vm.expectRevert(abi.encodeWithSelector(DeployAll.LinkMismatch.selector, s.targetRouter, "outbeIca"));
        script.checkLinks(s, c);
    }

    function test_OutbeAccount_RevertsWhenHyperlaneDoesNotKnowThisChain() public {
        script.setEnv(address(factory), SALT, false, address(0));
        vm.expectRevert(abi.encodeWithSelector(IntexScript.NoOutbeAccount.selector, uint32(block.chainid)));
        script.outbeAccount();
    }
}
