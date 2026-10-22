// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {ERC7786TokenBridge} from "../../src/ERC7786TokenBridge.sol";

enum SyntheticSource {
    Erc7802,
    TokenFactory
}

/// @dev Everything a route needs to be deployed, except the token's creation code. The creation
///      code cannot be data, since `type(T).creationCode` is a compile-time construct. Each route
///      file in `script/routes/` supplies both.
struct RouteSpec {
    /// @dev Salt label of the token, and - suffixed with `Bridge` - of its bridge. Part of both CREATE3 addresses:
    ///      changing it moves every deployment of this route.
    string tokenLabel;
    /// @dev Which chain holds the canonical token - Outbe (WCOEN) or the external chain (USDT).
    bool canonicalOnOutbe;
    /// @dev Env var naming an already-deployed canonical token to adopt instead of deploying one - the issuer's USDT
    ///      on a real network. Read only on the chain where this route is canonical.
    string canonicalTokenEnv;
    /// @dev Env var naming an already-deployed synthetic token to adopt - a factory-issued stablecoin, which CREATE3
    ///      cannot place. Read only on the chain where this route is synthetic.
    string syntheticTokenEnv;
    SyntheticSource syntheticSource;
}

/// @dev Route-agnostic half of the deployment: guards, salts, address prediction, and the deploy sequence itself.
///      It knows how to deploy *a* route, never which routes exist. See the sibling files in this
///      directory.
///
///      Addresses come from CREATE3, so they depend only on (factory, salt, deployer). They never
///      depend on the ERC-7786 hub, the bridge mode or the token metadata. That is what lets one
///      address hold the canonical token on one chain and the ERC-7802 synthetic on another. It
///      also puts each bridge at the same address on every chain, so the script knows every peer
///      before the remote chain is deployed.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `BRIDGE_ADDRESS`, `OUTBE_CHAIN_ID`, `EXTERNAL_CHAIN_ID`,
///   `REMOTE_CHAIN_IDS`, and `CONTROLLER_ICA_ADDRESS` on a chain other than Outbe.
/// Optional env: `CREATE3_FACTORY_ADDRESS`, the route's `canonicalTokenEnv`, `INITIAL_MINT_AMOUNT`,
///   `INITIAL_MINT_RECIPIENT`.
abstract contract BaseRoute is Script {
    /// @dev The HyperlaneController precompile. Governance votes drive it, so it owns the bridges on Outbe.
    address internal constant HYPERLANE_CONTROLLER = 0x000000000000000000000000000000000000EE14;

    error MissingCode(address target);
    error DomainTooLarge(uint256 chainId);
    error UndeclaredChain(uint256 chainId);
    error SyntheticTokenNotSet(string env);

    // ================================================ Env accessors ================================================

    function _pk() internal view returns (uint256) {
        return vm.envUint("DEPLOYER_PK");
    }

    /// @dev `virtual` so a test harness can act as the deployer. The deployer is part of every salt.
    ///      A harness that cannot override it cannot exercise a deployment at all.
    function _deployer() internal view virtual returns (address) {
        return vm.addr(_pk());
    }

    /// @dev Governance owns every bridge. On Outbe that is the HyperlaneController precompile. On
    ///      another chain it is the controller's Interchain Account, which the ICA router derives.
    function _bridgeOwner() internal view returns (address) {
        return _isOutbe() ? HYPERLANE_CONTROLLER : vm.envAddress("CONTROLLER_ICA_ADDRESS");
    }

    /// @dev One peer per `REMOTE_CHAIN_IDS` entry other than this chain. CREATE3 puts the peer at
    ///      `tokenBridge` on every chain.
    function _bridgePeers(address tokenBridge) internal view returns (ERC7786TokenBridge.Peer[] memory peers) {
        uint256[] memory remotes = vm.envUint("REMOTE_CHAIN_IDS", ",");
        uint256 count;
        for (uint256 i = 0; i < remotes.length; i++) {
            if (remotes[i] != block.chainid) count++;
        }
        peers = new ERC7786TokenBridge.Peer[](count);
        count = 0;
        for (uint256 i = 0; i < remotes.length; i++) {
            if (remotes[i] != block.chainid) {
                peers[count++] = ERC7786TokenBridge.Peer(_toDomain(remotes[i]), tokenBridge);
            }
        }
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

    /// @dev The factory namespaces this with its caller, so a third party cannot squat the address with
    ///      their own bytecode. Consequence: the operator must deploy every chain from the same
    ///      key, or the addresses diverge.
    function _salt(string memory label, string memory salt) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(label, salt));
    }

    function _tokenAddress(address factory, string memory salt, RouteSpec memory spec) internal view returns (address) {
        return Create3Factory(factory).predict(_deployer(), _salt(spec.tokenLabel, salt));
    }

    /// @dev Lets the configure and send scripts drop every address env var.
    function _bridgeAddress(address factory, string memory salt, RouteSpec memory spec)
        internal
        view
        returns (address)
    {
        return Create3Factory(factory).predict(_deployer(), _salt(string.concat(spec.tokenLabel, "Bridge"), salt));
    }

    // =================================================== Guards ====================================================

    function _requireCode(address target) internal view {
        if (target.code.length == 0) revert MissingCode(target);
    }

    function _toDomain(uint256 chainId) internal pure returns (uint32) {
        if (chainId > type(uint32).max) revert DomainTooLarge(chainId);
        return uint32(chainId);
    }

    /// @dev Nothing may be deployed onto a chain the operator did not declare as one end of a route. A wrong
    ///      `--rpc-url` is a mistake, not a deployment. Without this check, the connected chain
    ///      would silently count as "not Outbe", i.e. as the external end. Then a full set of
    ///      contracts would land on it.
    function _requireDeclaredChain() internal view {
        if (!_isGuardedChain()) revert UndeclaredChain(block.chainid);
    }

    function _isGuardedChain() internal view returns (bool) {
        uint256 externalChainId = vm.envOr("EXTERNAL_CHAIN_ID", uint256(0));
        if (externalChainId != 0 && block.chainid == externalChainId) return true;

        uint256 outbeChainId = vm.envOr("OUTBE_CHAIN_ID", uint256(0));
        return outbeChainId != 0 && block.chainid == outbeChainId;
    }

    // =================================================== Deploy ====================================================

    /// @dev Creation code plus constructor arguments of the token that this chain holds for `spec`.
    ///      A synthetic token takes `tokenBridge` as its fixed mint authority.
    function _tokenInitCode(RouteSpec memory spec, bool canonical, address tokenBridge)
        internal
        virtual
        returns (bytes memory);

    /// @dev Deploys one route's token and bridge on the connected chain. Everything comes from `spec`.
    ///      Each step checks on-chain state first. Thus a re-run is a no-op, and this function
    ///      completes a partially finished chain rather than skipping it.
    ///
    ///      This function opens no broadcast on purpose. The entrypoints open a single one, and the
    ///      tests call this function directly.
    function _deployRoute(address factory, string memory salt, RouteSpec memory spec)
        internal
        returns (address token, address tokenBridge)
    {
        _requireDeclaredChain();

        bool canonical = _isCanonicalHere(spec);
        address hub = vm.envAddress("BRIDGE_ADDRESS");
        _requireCode(hub);

        bytes32 tokenSalt = _salt(spec.tokenLabel, salt);
        bytes32 bridgeSalt = _salt(string.concat(spec.tokenLabel, "Bridge"), salt);

        // The two candidates for the token address, resolved independently:
        //   - `configured`: it already exists and is not ours to place. It is the issuer's USDT on
        //     a real network, or a factory-issued stablecoin on Outbe. Its address is then whatever
        //     the operator says.
        //   - `predicted`: where CREATE3 will put it if this script places it.
        address configured = vm.envOr(canonical ? spec.canonicalTokenEnv : spec.syntheticTokenEnv, address(0));
        address predicted = Create3Factory(factory).predict(_deployer(), tokenSalt);
        // Known before the bridge exists: CREATE3 ignores the constructor arguments. A synthetic
        // token takes it as its fixed mint authority.
        tokenBridge = Create3Factory(factory).predict(_deployer(), bridgeSalt);

        if (configured != address(0)) {
            token = configured;
            _requireCode(token);
        } else if (!canonical && spec.syntheticSource == SyntheticSource.TokenFactory) {
            // Issued by governance, never by this script, so it must be named or nothing.
            revert SyntheticTokenNotSet(spec.syntheticTokenEnv);
        } else {
            token = predicted;
            if (token.code.length == 0) {
                Create3Factory(factory).deploy(tokenSalt, _tokenInitCode(spec, canonical, tokenBridge));
            }
        }

        if (tokenBridge.code.length == 0) {
            bytes memory args =
                abi.encode(token, hub, _bridgeMode(spec, canonical), _bridgeOwner(), _bridgePeers(tokenBridge));
            Create3Factory(factory).deploy(bridgeSalt, abi.encodePacked(type(ERC7786TokenBridge).creationCode, args));
        }
    }

    /// @dev The canonical side always takes custody. The synthetic side follows how its token is
    ///      issued.
    function _bridgeMode(RouteSpec memory spec, bool canonical)
        internal
        pure
        returns (ERC7786TokenBridge.TokenBridgeMode)
    {
        if (canonical) return ERC7786TokenBridge.TokenBridgeMode.LockUnlock;
        if (spec.syntheticSource == SyntheticSource.Erc7802) return ERC7786TokenBridge.TokenBridgeMode.BurnMint;
        return ERC7786TokenBridge.TokenBridgeMode.IssuerBurnMint;
    }

    function _logRoute(string memory prefix, address token, address tokenBridge) internal pure {
        console2.log(string.concat(prefix, "_TOKEN="), token);
        console2.log(string.concat(prefix, "_BRIDGE="), tokenBridge);
    }
}
