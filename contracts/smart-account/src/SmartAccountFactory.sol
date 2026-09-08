// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

import {ISmartAccountFactory} from "./interfaces/ISmartAccountFactory.sol";
import {IKernelFactory} from "./interfaces/kernel/IKernelFactory.sol";
import {BundleModulePlugin} from "./BundleModulePlugin.sol";
import {ITokenBundle} from "./interfaces/ITokenBundle.sol";
import {GuardedKernel} from "./kernel/GuardedKernel.sol";
import {Kernel} from "./kernel/guarded/Kernel.sol";
import {Install} from "@zerodev/kernel/types/Structs.sol";
import {
    MODULE_TYPE_POLICY,
    MODULE_TYPE_SIGNER,
    MODULE_TYPE_HOOK,
    MODULE_TYPE_FALLBACK
} from "@zerodev/kernel/types/Constants.sol";
import {IERC20Metadata} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

/// @notice Owner-only deterministic accounts with a separate root-authorized bundle installation.
contract SmartAccountFactory is ISmartAccountFactory, ReentrancyGuard {
    address public immutable override kernelFactory;
    address public immutable override sudoPolicy;
    address public immutable override bundleModulePlugin;
    address public immutable override ecdsaSigner;
    address public immutable override bundleWithdrawHook;
    mapping(address => bool) public accounts;
    bytes4 public constant OWNER_PERMISSION = bytes4(keccak256("credis.owner"));
    error InvalidConfiguration();
    error UnknownAccount();
    error BundleNotUnopened();

    constructor(address kernelFactory_, address sudoPolicy_, address plugin_, address signer_, address withdrawHook_) {
        require(
            kernelFactory_.code.length > 0 && sudoPolicy_.code.length > 0 && plugin_.code.length > 0
                && signer_.code.length > 0 && withdrawHook_.code.length > 0,
            InvalidConfiguration()
        );
        require(
            address(GuardedKernel(payable(IKernelFactory(kernelFactory_).UUPS())).custody()) == plugin_,
            InvalidConfiguration()
        );
        kernelFactory = kernelFactory_;
        sudoPolicy = sudoPolicy_;
        bundleModulePlugin = plugin_;
        ecdsaSigner = signer_;
        bundleWithdrawHook = withdrawHook_;
    }

    function createAccount(address owner, uint256 salt) external override returns (address account) {
        account = IKernelFactory(kernelFactory).deploy(_ownerPackages(owner), salt);
        accounts[account] = true;
    }

    function getAccountAddress(address owner, uint256 salt) external view override returns (address) {
        return IKernelFactory(kernelFactory).getAddress(_ownerPackages(owner), salt);
    }

    function openBundle(
        address account,
        address cca,
        address[] calldata tokens,
        address[] calldata senders,
        uint256 nonce,
        bytes calldata signature
    ) external override nonReentrant {
        require(accounts[account], UnknownAccount());
        BundleModulePlugin plugin = BundleModulePlugin(bundleModulePlugin);
        require(plugin.status(account) == ITokenBundle.Status.Unopened, BundleNotUnopened());
        Install[] memory packages = getBundleInstallPackages(cca, tokens, senders);
        Kernel(payable(account)).installModule(false, nonce, packages, signature);
        plugin.openBundle(account, cca, tokens, senders);
    }

    function getBundleInstallPackages(address cca, address[] calldata tokens, address[] calldata senders)
        public
        view
        override
        returns (Install[] memory packages)
    {
        require(cca != address(0) && tokens.length != 0 && senders.length != 0, InvalidConfiguration());
        for (uint256 i; i < senders.length; ++i) {
            require(senders[i] != address(0), InvalidConfiguration());
            for (uint256 j; j < i; ++j) {
                require(senders[i] != senders[j], InvalidConfiguration());
            }
        }
        packages = new Install[](2 + 2 * tokens.length);
        packages[0] = Install(MODULE_TYPE_HOOK, bundleWithdrawHook, "", "");
        // Read-only routing; funding is authenticated directly by custody, never by the account.
        packages[1] = Install(
            MODULE_TYPE_FALLBACK,
            bundleModulePlugin,
            abi.encode(cca, tokens, senders),
            abi.encodePacked(BundleModulePlugin.bundleBalance.selector, bytes1(0), bytes20(address(1)))
        );
        for (uint256 i; i < tokens.length; ++i) {
            require(tokens[i].code.length > 0 && IERC20Metadata(tokens[i]).decimals() == 6, InvalidConfiguration());
            bytes4 id = ccaPermission(tokens[i]);
            require(id != OWNER_PERMISSION && id != bytes4(0), InvalidConfiguration());
            for (uint256 j; j < i; ++j) {
                require(id != ccaPermission(tokens[j]), InvalidConfiguration());
            }
            packages[2 + 2 * i] =
                Install(MODULE_TYPE_POLICY, sudoPolicy, abi.encodePacked(bytes32(id)), abi.encodePacked(id));
            packages[3 + 2 * i] = Install(
                MODULE_TYPE_SIGNER,
                ecdsaSigner,
                abi.encodePacked(bytes32(id), cca),
                abi.encodePacked(id, bundleWithdrawHook, Kernel.execute.selector)
            );
        }
    }

    function ccaPermission(address token) public pure returns (bytes4) {
        return bytes4(keccak256(abi.encode("credis.cca", token)));
    }

    function _ownerPackages(address owner) private view returns (Install[] memory packages) {
        require(owner != address(0), InvalidConfiguration());
        packages = new Install[](2);
        packages[0] = Install(
            MODULE_TYPE_POLICY,
            sudoPolicy,
            abi.encodePacked(bytes32(OWNER_PERMISSION)),
            abi.encodePacked(OWNER_PERMISSION)
        );
        packages[1] = Install(
            MODULE_TYPE_SIGNER,
            ecdsaSigner,
            abi.encodePacked(bytes32(OWNER_PERMISSION), owner),
            abi.encodePacked(OWNER_PERMISSION, bytes20(address(0)), Kernel.execute.selector)
        );
    }
}
