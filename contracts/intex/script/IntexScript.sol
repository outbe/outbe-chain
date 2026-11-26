// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {Create3Proxy} from "@shared/Create3Proxy.sol";
import {SALT_VERSION} from "@shared/SaltVersion.sol";
import {CROSSCHAIN_BRIDGE, ICrosschainBridge} from "@precompiles/ICrosschainBridge.sol";
import {IntexCreate3} from "./IntexCreate3.sol";

/// @dev What every Intex script reads: the CREATE3 factory, the salt, the five predicted proxies and
///      the accounts of Outbe on this chain.
///      Env: `CREATE3_FACTORY_ADDRESS`, `CONTRACT_SALT` (blank or unset: `SALT_VERSION`),
///      `OUTBE_CHAIN_ID`, and `OUTBE_RPC_URL` where the Interchain Account of Outbe is read.
abstract contract IntexScript is Script {
    error DomainTooLarge(uint256 chainId);
    error AddressMismatch(string label, address predicted, address deployed);
    error NoOutbeAccount(uint32 domain);

    function create3Factory() public view virtual returns (address factory) {
        factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        require(factory.code.length != 0, "Create3Factory not deployed - deploy it from contracts/shared first");
    }

    function saltVersion() public view virtual returns (string memory version) {
        version = vm.envOr("CONTRACT_SALT", string(""));
        if (bytes(version).length == 0) version = SALT_VERSION;
    }

    /// @dev The five Intex proxies of `deployer` on this chain, deployed or not.
    function stack(address deployer) public view returns (IntexCreate3.Stack memory) {
        return IntexCreate3.stack(create3Factory(), deployer, saltVersion());
    }

    function onOutbe() public view virtual returns (bool) {
        return block.chainid == vm.envUint("OUTBE_CHAIN_ID");
    }

    function outbeChainId() public view returns (uint32) {
        uint256 chainId = vm.envUint("OUTBE_CHAIN_ID");
        if (chainId > type(uint32).max) revert DomainTooLarge(chainId);
        return uint32(chainId);
    }

    /// @dev Governance on this chain: CrosschainBridge on Outbe, its Interchain Account elsewhere.
    function governanceOwner() public returns (address) {
        if (onOutbe()) return CROSSCHAIN_BRIDGE;
        return outbeAccount();
    }

    /// @dev The Interchain Account of Outbe on this chain, Outbe included. HyperlaneController must
    ///      know this chain and its router first.
    function outbeAccount() public returns (address account) {
        if (block.chainid > type(uint32).max) revert DomainTooLarge(block.chainid);
        uint32 domain = uint32(block.chainid);
        account = _remoteAccount(domain);
        if (account == address(0)) revert NoOutbeAccount(domain);
    }

    /// @dev Deploys `implementation` behind the proxy at `predicted`. Requires an active broadcast
    ///      from the deployer `predicted` was derived for.
    function _deployProxy(string memory label, address predicted, address implementation, bytes memory initData)
        internal
    {
        address proxy = Create3Proxy.deploy(
            Create3Factory(create3Factory()), IntexCreate3.salt(label, saltVersion()), implementation, initData
        );
        if (proxy != predicted) revert AddressMismatch(label, predicted, proxy);
    }

    /// @dev Asks CrosschainBridge on Outbe for its Interchain Account on `domain`. A fork cannot run
    ///      the precompile, so this is a raw `eth_call` to the Outbe node. `virtual` so a test
    ///      harness can supply the account.
    function _remoteAccount(uint32 domain) internal virtual returns (address) {
        string memory params = string.concat(
            '[{"to":"',
            vm.toString(CROSSCHAIN_BRIDGE),
            '","data":"',
            vm.toString(abi.encodeCall(ICrosschainBridge.remoteAccount, (domain))),
            '"},"latest"]'
        );
        return abi.decode(vm.rpc(vm.envString("OUTBE_RPC_URL"), "eth_call", params), (address));
    }
}
