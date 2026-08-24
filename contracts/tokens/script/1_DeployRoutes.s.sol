// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

import {CreateX} from "./0_DeployCreateX.s.sol";
import {ConfigurableERC7802} from "../src/ConfigurableERC7802.sol";
import {ERC7786TokenBridge} from "../src/ERC7786TokenBridge.sol";
import {USDT} from "../src/native/USDT.sol";
import {WCOEN} from "../src/native/WCOEN.sol";
import {BridgeableERC20} from "../src/synthetic/BridgeableERC20.sol";
import {BridgeableERC20Stable} from "../src/synthetic/BridgeableERC20Stable.sol";

/// @dev Shared state and guards for the token-route scripts. Addresses come from CREATE3, so they depend only on
///      (factory, salt) — never on the owner, the ERC-7786 hub, the bridge mode or the token metadata. That is what
///      lets one address hold the canonical token on one chain and the ERC-7802 synthetic on another.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `BRIDGE_ADDRESS`, `OUTBE_CHAIN_ID`, `EXTERNAL_CHAIN_ID`.
/// Optional env: `OWNER_ADDRESS` (default: deployer), `ALLOW_EOA_OWNER`, `CREATEX_ADDRESS`,
///   `EXTERNAL_USDT_TOKEN` / `OUTBE_WCOEN_TOKEN` (use a pre-existing canonical token),
///   `INITIAL_MINT_AMOUNT`, `INITIAL_MINT_RECIPIENT`.
abstract contract TokenDeployBase is Script {
    /// @dev A route slot, not an implementation: `USDT` means "whatever represents USDT on this chain" — the mock on
    ///      the external chain, the ERC-7802 synthetic on Outbe.
    enum Route {
        USDT,
        WCOEN
    }

    string internal constant USDT_TOKEN_LABEL = "USDT";
    string internal constant USDT_BRIDGE_LABEL = "USDTBridge";
    string internal constant WCOEN_TOKEN_LABEL = "WCOEN";
    string internal constant WCOEN_BRIDGE_LABEL = "WCOENBridge";

    bytes4 internal constant SET_TOKEN_BRIDGE_SELECTOR = bytes4(keccak256("setTokenBridge(address)"));

    error MissingCode(address target);
    error UnauthorizedSigner(address signer, address expectedOwner);
    error OwnerMustBeMultisigContract(address owner, uint256 chainId);
    error DomainTooLarge(uint256 chainId);
    error MockUSDTDeploymentNotAllowed(uint256 chainId);

    // ================================================ Env accessors ================================================

    function _pk() internal view returns (uint256) {
        return vm.envUint("DEPLOYER_PK");
    }

    /// @dev `virtual` so a test harness can act as the deployer: the deployer both signs the owner-only calls and
    ///      is part of every salt, so a harness that cannot override it cannot exercise `deployRoute` at all.
    function _deployer() internal view virtual returns (address) {
        return vm.addr(_pk());
    }

    function _owner() internal view returns (address) {
        address owner = vm.envOr("OWNER_ADDRESS", address(0));
        return owner != address(0) ? owner : _deployer();
    }

    function _mintRecipient() internal view returns (address) {
        address recipient = vm.envOr("INITIAL_MINT_RECIPIENT", address(0));
        return recipient != address(0) ? recipient : _deployer();
    }

    /// @dev The chain is Outbe iff it is the chain the operator declared as Outbe. `envUint`, not `envOr`: an unset
    ///      value must not silently mean "external" and deploy the Outbe half onto the wrong chain.
    function _isOutbe() internal view returns (bool) {
        return block.chainid == vm.envUint("OUTBE_CHAIN_ID");
    }

    /// @dev USDT is canonical on the external chain; WCOEN is canonical on Outbe.
    function _isCanonicalHere(Route route) internal view returns (bool) {
        return _isOutbe() == (route == Route.WCOEN);
    }

    /// @dev A canonical token that already exists on this chain — a real USDT on a real network. Only the token half
    ///      of the "one address everywhere" property is given up: the bridge address stays identical, because the
    ///      token only enters the bridge's constructor args and CREATE3 ignores those.
    function _configuredCanonicalToken(Route route) internal view returns (address) {
        return
            route == Route.USDT
                ? vm.envOr("EXTERNAL_USDT_TOKEN", address(0))
                : vm.envOr("OUTBE_WCOEN_TOKEN", address(0));
    }

    // ==================================================== Salt =====================================================

    /// @dev The deployer is part of the salt so a third party cannot squat the address with their own bytecode.
    ///      Consequence: every chain must be deployed from the same key, or the addresses diverge.
    function _saltHash(string memory label, string memory salt, address deployer) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(label, salt, deployer));
    }

    function _labels(Route route) internal pure returns (string memory tokenLabel, string memory bridgeLabel) {
        return route == Route.USDT ? (USDT_TOKEN_LABEL, USDT_BRIDGE_LABEL) : (WCOEN_TOKEN_LABEL, WCOEN_BRIDGE_LABEL);
    }

    function _tokenAddress(address createX, string memory salt, Route route) internal view returns (address) {
        (string memory tokenLabel,) = _labels(route);
        return CreateX(createX).computeCreate3Address(_saltHash(tokenLabel, salt, _deployer()));
    }

    /// @dev Lets the configure and send scripts drop every address env var.
    function _bridgeAddress(address createX, string memory salt, Route route) internal view returns (address) {
        (, string memory bridgeLabel) = _labels(route);
        return CreateX(createX).computeCreate3Address(_saltHash(bridgeLabel, salt, _deployer()));
    }

    // =================================================== Guards ====================================================

    function _requireCode(address target) internal view {
        if (target.code.length == 0) revert MissingCode(target);
    }

    function _requireOwner(address signer, address expectedOwner) internal pure {
        if (signer != expectedOwner) revert UnauthorizedSigner(signer, expectedOwner);
    }

    function _toDomain(uint256 chainId) internal pure returns (uint32) {
        if (chainId > type(uint32).max) revert DomainTooLarge(chainId);
        return uint32(chainId);
    }

    /// @dev The mock USDT stands in for a canonical USDT that does not exist on the external chain, so it may only be
    ///      deployed on the chain the operator declared as the external chain. Any other chain (a wrong `--rpc-url`,
    ///      a mainnet) is a mistake, not a deployment.
    function _requireMockUSDTDeploymentAllowed() internal view {
        // An unset `EXTERNAL_CHAIN_ID` reads as 0, which no chain reports, so it fails this check too.
        if (block.chainid != vm.envOr("EXTERNAL_CHAIN_ID", uint256(0))) {
            revert MockUSDTDeploymentNotAllowed(block.chainid);
        }
    }

    function _isGuardedChain() internal view returns (bool) {
        uint256 externalChainId = vm.envOr("EXTERNAL_CHAIN_ID", uint256(0));
        if (externalChainId != 0 && block.chainid == externalChainId) return true;

        uint256 outbeChainId = vm.envOr("OUTBE_CHAIN_ID", uint256(0));
        return outbeChainId != 0 && block.chainid == outbeChainId;
    }

    function _requireContractOwnerOnGuardedChain(address owner) internal view {
        _requireContractOwnerOnGuardedChain(owner, vm.envOr("ALLOW_EOA_OWNER", false));
    }

    function _requireContractOwnerOnGuardedChain(address owner, bool allowEoaOwner) internal view {
        if (_isGuardedChain() && owner.code.length == 0 && !allowEoaOwner) {
            revert OwnerMustBeMultisigContract(owner, block.chainid);
        }
    }

    function _requireBridgeOwnerOnGuardedChain(address tokenBridge) internal view {
        _requireContractOwnerOnGuardedChain(Ownable(tokenBridge).owner());
    }

    // ================================================ Owner calls ==================================================

    function _shouldBroadcastOwnerCall(
        address signer,
        address owner,
        address target,
        bytes memory data,
        string memory description
    ) internal view returns (bool) {
        if (signer == owner) return true;
        if (owner.code.length != 0) {
            _logSafeTransaction(description, owner, target, data);
            return false;
        }

        _requireOwner(signer, owner);
        return false;
    }

    function _logSafeTransaction(string memory description, address safe, address target, bytes memory data)
        internal
        pure
    {
        console2.log(description);
        console2.log("Safe owner detected; submit this transaction through the owner Safe:");
        console2.log("  safe=", safe);
        console2.log("  to=", target);
        console2.log("  value=0");
        console2.log("  data=");
        console2.logBytes(data);
    }

    // =================================================== Deploy ====================================================

    /// @dev Deploys the token and the bridge of one route on the connected chain. No broadcast is opened here on
    ///      purpose: `DeployAll` opens a single one, and the tests call this directly.
    function deployRoute(address createX, string memory salt, Route route)
        public
        returns (address token, address tokenBridge)
    {
        address owner = _owner();
        bool canonical = _isCanonicalHere(route);
        (string memory tokenLabel, string memory bridgeLabel) = _labels(route);

        _requireContractOwnerOnGuardedChain(owner);
        address hub = vm.envAddress("BRIDGE_ADDRESS");
        _requireCode(hub);

        bytes32 tokenSalt = _saltHash(tokenLabel, salt, _deployer());
        bytes32 bridgeSalt = _saltHash(bridgeLabel, salt, _deployer());

        // Both addresses are known before either contract exists, so the bridge's immutable `token_` and the token's
        // `setTokenBridge` no longer impose a deploy order — only a code-existence one.
        token = CreateX(createX).computeCreate3Address(tokenSalt);
        tokenBridge = CreateX(createX).computeCreate3Address(bridgeSalt);

        address existing = canonical ? _configuredCanonicalToken(route) : address(0);
        if (existing != address(0)) {
            token = existing;
            _requireCode(token);
        } else if (token.code.length == 0) {
            if (route == Route.USDT && canonical) _requireMockUSDTDeploymentAllowed();
            CreateX(createX).deployCreate3(tokenSalt, _tokenInitCode(route, canonical, owner));
            if (route == Route.USDT && canonical) {
                USDT(token).mint(_mintRecipient(), vm.envOr("INITIAL_MINT_AMOUNT", uint256(1_000_000_000e6)));
            }
        }

        if (tokenBridge.code.length == 0) {
            ERC7786TokenBridge.TokenBridgeMode mode =
                canonical ? ERC7786TokenBridge.TokenBridgeMode.LockUnlock : ERC7786TokenBridge.TokenBridgeMode.BurnMint;
            CreateX(createX)
                .deployCreate3(
                    bridgeSalt,
                    abi.encodePacked(type(ERC7786TokenBridge).creationCode, abi.encode(token, hub, owner, mode))
                );
        }
        _requireBridgeOwnerOnGuardedChain(tokenBridge);

        // Only the ERC-7802 side has a token->bridge link, and it must be set after the bridge exists.
        if (existing == address(0) && !canonical) _setTokenBridge(token, tokenBridge, tokenLabel);
    }

    /// @dev The only per-route difference left. Not `view`: with `dynamic_test_linking` on,
    ///      `type(T).creationCode` compiles to a state-modifying `vm.getCode()` cheatcode call.
    function _tokenInitCode(Route route, bool canonical, address owner) internal returns (bytes memory) {
        if (route == Route.USDT) {
            return canonical
                ? type(USDT).creationCode
                : abi.encodePacked(
                    type(BridgeableERC20Stable).creationCode, abi.encode("USDT0", "USDT0", uint8(6), uint16(840), owner)
                );
        }
        return canonical
            ? type(WCOEN).creationCode
            : abi.encodePacked(type(BridgeableERC20).creationCode, abi.encode("Wrapped COEN", "WCOEN", uint8(6), owner));
    }

    function _setTokenBridge(address token, address tokenBridge, string memory label) internal {
        _requireCode(token);
        _requireCode(tokenBridge);

        ConfigurableERC7802 synthetic = ConfigurableERC7802(token);
        if (synthetic.tokenBridge() == tokenBridge) return;

        address owner = synthetic.owner();
        _requireContractOwnerOnGuardedChain(owner);

        bytes memory data = abi.encodeWithSelector(SET_TOKEN_BRIDGE_SELECTOR, tokenBridge);
        if (!_shouldBroadcastOwnerCall(_deployer(), owner, token, data, "Set token bridge")) return;

        synthetic.setTokenBridge(tokenBridge);
        console2.log(string.concat(label, " token bridge set:"), tokenBridge);
    }

    function _logRoute(string memory prefix, address token, address tokenBridge) internal pure {
        console2.log(string.concat(prefix, "_TOKEN="), token);
        console2.log(string.concat(prefix, "_BRIDGE="), tokenBridge);
    }
}

/// @dev Deploys both routes on the connected chain. Which side of each route is canonical is derived from
///      `OUTBE_CHAIN_ID`, so the same command runs on every chain.
contract DeployRoutes is TokenDeployBase {
    function run() public virtual {
        string memory salt = vm.envString("CONTRACT_SALT");
        address createX = vm.envAddress("CREATEX_ADDRESS");

        vm.startBroadcast(_pk());
        (address usdt, address usdtBridge) = deployRoute(createX, salt, Route.USDT);
        (address wcoen, address wcoenBridge) = deployRoute(createX, salt, Route.WCOEN);
        vm.stopBroadcast();

        _logRoute("USDT", usdt, usdtBridge);
        _logRoute("WCOEN", wcoen, wcoenBridge);
    }
}
