// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

import {CreateX} from "../0_DeployCreateX.s.sol";
import {BridgeableERC20} from "../../src/synthetic/BridgeableERC20.sol";
import {ERC7786TokenBridge} from "../../src/ERC7786TokenBridge.sol";

/// @dev Everything a route needs to be deployed, except the token's creation code - that one cannot be data, since
///      `type(T).creationCode` is a compile-time construct. Each route file in `script/routes/` supplies both.
struct RouteSpec {
    /// @dev Salt label of the token. Part of the CREATE3 address: changing it moves every deployment of this route.
    string tokenLabel;
    /// @dev Salt label of the bridge. Same warning.
    string bridgeLabel;
    /// @dev Which chain holds the canonical token - Outbe (WCOEN) or the external chain (USDT).
    bool canonicalOnOutbe;
    /// @dev Env var naming an already-deployed canonical token to adopt instead of deploying one - the issuer's USDT
    ///      on a real network. Read only on the chain where this route is canonical.
    string canonicalTokenEnv;
}

/// @dev Route-agnostic half of the deployment: guards, salts, address prediction, and the deploy sequence itself.
///      It knows how to deploy *a* route, never which routes exist - see the sibling files in this directory.
///
///      Addresses come from CREATE3, so they depend only on (factory, salt, deployer) - never on the owner, the
///      ERC-7786 hub, the bridge mode or the token metadata. That is what lets one address hold the canonical token
///      on one chain and the ERC-7802 synthetic on another.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `BRIDGE_ADDRESS`, `OUTBE_CHAIN_ID`, `EXTERNAL_CHAIN_ID`.
/// Optional env: `OWNER_ADDRESS` (default: deployer), `ALLOW_EOA_OWNER`, `CREATEX_ADDRESS`,
///   the route's `canonicalTokenEnv`, `INITIAL_MINT_AMOUNT`, `INITIAL_MINT_RECIPIENT`.
abstract contract BaseRoute is Script {
    bytes4 internal constant SET_TOKEN_BRIDGE_SELECTOR = bytes4(keccak256("setTokenBridge(address)"));

    error MissingCode(address target);
    error UnauthorizedSigner(address signer, address expectedOwner);
    error OwnerMustBeMultisigContract(address owner, uint256 chainId);
    error DomainTooLarge(uint256 chainId);
    error UndeclaredChain(uint256 chainId);

    // ================================================ Env accessors ================================================

    function _pk() internal view returns (uint256) {
        return vm.envUint("DEPLOYER_PK");
    }

    /// @dev `virtual` so a test harness can act as the deployer: the deployer both signs the owner-only calls and is
    ///      part of every salt, so a harness that cannot override it cannot exercise a deployment at all.
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

    function _isCanonicalHere(RouteSpec memory spec) internal view returns (bool) {
        return _isOutbe() == spec.canonicalOnOutbe;
    }

    // ==================================================== Salt =====================================================

    /// @dev The deployer is part of the salt so a third party cannot squat the address with their own bytecode.
    ///      Consequence: every chain must be deployed from the same key, or the addresses diverge.
    function _saltHash(string memory label, string memory salt, address deployer) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(label, salt, deployer));
    }

    function _tokenAddress(address createX, string memory salt, RouteSpec memory spec) internal view returns (address) {
        return CreateX(createX).computeCreate3Address(_saltHash(spec.tokenLabel, salt, _deployer()));
    }

    /// @dev Lets the configure and send scripts drop every address env var.
    function _bridgeAddress(address createX, string memory salt, RouteSpec memory spec)
        internal
        view
        returns (address)
    {
        return CreateX(createX).computeCreate3Address(_saltHash(spec.bridgeLabel, salt, _deployer()));
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

    /// @dev Nothing may be deployed onto a chain the operator did not declare as one end of a route. A wrong
    ///      `--rpc-url` is a mistake, not a deployment - and without this check the connected chain would silently
    ///      count as "not Outbe", i.e. as the external end, and a full set of contracts would land on it.
    function _requireDeclaredChain() internal view {
        if (!_isGuardedChain()) revert UndeclaredChain(block.chainid);
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

    /// @dev Deploys one route's token and bridge on the connected chain. The caller supplies the token's creation
    ///      code because `type(T).creationCode` cannot be carried in a struct; everything else comes from `spec`.
    ///      Each step checks on-chain state first, so a re-run is a no-op and a partially finished chain (the Safe
    ///      has not executed `setTokenBridge` yet) is completed rather than skipped.
    ///
    ///      No broadcast is opened here on purpose: the entrypoints open a single one, and the tests call this
    ///      directly.
    function _deployRoute(address createX, string memory salt, RouteSpec memory spec, bytes memory tokenInitCode)
        internal
        returns (address token, address tokenBridge)
    {
        _requireDeclaredChain();

        address owner = _owner();
        bool canonical = _isCanonicalHere(spec);

        _requireContractOwnerOnGuardedChain(owner);
        address hub = vm.envAddress("BRIDGE_ADDRESS");
        _requireCode(hub);

        bytes32 tokenSalt = _saltHash(spec.tokenLabel, salt, _deployer());
        bytes32 bridgeSalt = _saltHash(spec.bridgeLabel, salt, _deployer());

        // Both addresses are known before either contract exists, so the bridge's immutable `token_` and the token's
        // `setTokenBridge` no longer impose a deploy order - only a code-existence one.
        token = CreateX(createX).computeCreate3Address(tokenSalt);
        tokenBridge = CreateX(createX).computeCreate3Address(bridgeSalt);

        // A canonical token may already exist and not be ours to place - the issuer's USDT on a real network. Its
        // address is then whatever it is, but the bridge address stays deterministic: the token only enters the
        // bridge's constructor args, and CREATE3 ignores those.
        address existing = canonical ? vm.envOr(spec.canonicalTokenEnv, address(0)) : address(0);
        if (existing != address(0)) {
            token = existing;
            _requireCode(token);
        } else if (token.code.length == 0) {
            CreateX(createX).deployCreate3(tokenSalt, tokenInitCode);
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
        if (existing == address(0) && !canonical) _setTokenBridge(token, tokenBridge, spec.tokenLabel);
    }

    function _setTokenBridge(address token, address tokenBridge, string memory label) internal {
        _requireCode(token);
        _requireCode(tokenBridge);

        BridgeableERC20 synthetic = BridgeableERC20(token);
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
