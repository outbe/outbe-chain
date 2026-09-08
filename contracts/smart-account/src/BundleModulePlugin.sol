// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {IModule} from "@zerodev/kernel/interfaces/IERC7579Modules.sol";
import {MODULE_TYPE_FALLBACK} from "@zerodev/kernel/types/Constants.sol";
import {ICca} from "@precompiles/ICca.sol";
import {ITokenBundle} from "./interfaces/ITokenBundle.sol";

/// @notice Matched bundle reserves, isolated from the owner's Kernel execution authority.
contract BundleModulePlugin is IModule, ITokenBundle, EIP712, ReentrancyGuard {
    using SafeERC20 for IERC20;

    address public constant CCA_REGISTRY = 0x0000000000000000000000000000000000001011;
    uint256 public constant DAILY_LIMIT = 1000e6;
    uint256 public constant LIMIT_INTERVAL = 1 days;
    bytes32 public constant PAYMENT_TYPEHASH = keccak256(
        "Payment(address account,address token,address recipient,uint256 amount,uint256 nonce,uint256 deadline)"
    );
    address private immutable OWNER;
    address public factory;
    mapping(address => Status) public override status;
    mapping(address => address) public override linkedCca;
    mapping(address => uint256) public override paymentNonce;
    mapping(address => address[]) private tokens;
    mapping(address => address[]) private senders;
    mapping(address => mapping(address => uint256)) private balances;

    struct Window {
        uint256 used;
        uint256 end;
    }
    mapping(address => mapping(address => Window)) public windows;

    error Unauthorized();
    error AlreadyConfigured();
    error BundleNotOpen();
    error BundleNotUnopened();
    error HasBundleBalance(address token);
    error TokenNotInBundle(address token);
    error InvalidPayment();
    error CcaNotActive(address cca, ICca.State state);
    error InsufficientReserve();
    error WithdrawalLimitExceeded();
    error NonExactTransfer();

    event BundleOpened(address indexed account, address indexed cca, address[] tokens, address[] senders);
    event BundleClosed(address indexed account);
    event FactoryConfigured(address indexed factory);
    event BundleTransfer(address indexed from, address indexed to, address indexed token, uint256 value);
    event BundlePayment(address indexed account, address indexed token, address indexed recipient, uint256 amount);

    constructor(address owner_) EIP712("OutbeBundle", "1") {
        require(owner_ != address(0), Unauthorized());
        OWNER = owner_;
    }

    function setFactory(address factory_) external {
        require(msg.sender == OWNER && factory_.code.length > 0, Unauthorized());
        require(factory == address(0), AlreadyConfigured());
        factory = factory_;
        emit FactoryConfigured(factory_);
    }

    /// @dev The factory calls this only after the root-signed package installation succeeds.
    function openBundle(address account, address cca, address[] calldata tokens_, address[] calldata senders_)
        external
        nonReentrant
    {
        require(msg.sender == factory, Unauthorized());
        require(status[account] == Status.Unopened, BundleNotUnopened());
        _active(cca);
        tokens[account] = tokens_;
        senders[account] = senders_;
        linkedCca[account] = cca;
        status[account] = Status.Open;
        emit BundleOpened(account, cca, tokens_, senders_);
    }

    /// @notice Permanently retires the calling account, including an unopened account.
    function retire() external nonReentrant {
        _requireEmpty(msg.sender);
        if (status[msg.sender] != Status.Closed) {
            status[msg.sender] = Status.Closed;
            emit BundleClosed(msg.sender);
        }
    }

    function onInstall(bytes calldata) external payable override {}

    function onUninstall(bytes calldata) external payable override {
        _requireEmpty(msg.sender);
    }

    function isModuleType(uint256 id) external pure override returns (bool) {
        return id == MODULE_TYPE_FALLBACK;
    }

    function isInitialized(address account) external view override returns (bool) {
        return status[account] == Status.Open;
    }

    /// @notice Account fallback view. Custody funding uses topUpFor directly.
    function bundleBalance(address token) external view returns (uint256) {
        return balances[msg.sender][token];
    }

    function topUpFor(address account, address token, uint256 amount) external override nonReentrant {
        require(_allowed(account, msg.sender), Unauthorized());
        _topUp(account, msg.sender, token, amount);
    }

    function _topUp(address account, address sender, address token, uint256 amount) private {
        require(status[account] == Status.Open, BundleNotOpen());
        require(isBundleToken(account, token), TokenNotInBundle(token));
        require(amount != 0, InvalidPayment());
        IERC20 asset = IERC20(token);
        uint256 beforeBalance = asset.balanceOf(address(this));
        asset.safeTransferFrom(sender, address(this), amount);
        require(asset.balanceOf(address(this)) == beforeBalance + amount, NonExactTransfer());
        asset.safeTransferFrom(account, address(this), amount);
        require(asset.balanceOf(address(this)) == beforeBalance + 2 * amount, NonExactTransfer());
        balances[account][token] += 2 * amount;
        emit BundleTransfer(sender, account, token, amount);
    }

    function spend(Payment calldata p, bytes calldata signature) external override nonReentrant {
        require(msg.sender == p.account && status[p.account] == Status.Open, Unauthorized());
        require(isBundleToken(p.account, p.token), TokenNotInBundle(p.token));
        require(
            p.recipient != address(0) && p.recipient != address(this) && p.amount != 0 && p.deadline >= block.timestamp
                && p.nonce == paymentNonce[p.account],
            InvalidPayment()
        );
        address cca = linkedCca[p.account];
        _active(cca);
        require(ECDSA.recover(paymentDigest(p), signature) == cca, Unauthorized());
        uint256 debit = p.amount * 2;
        require(balances[p.account][p.token] >= debit, InsufficientReserve());
        Window storage window = windows[p.account][p.token];
        if (block.timestamp >= window.end) window.used = 0;
        window.end = block.timestamp + LIMIT_INTERVAL;
        require(window.used + p.amount <= DAILY_LIMIT, WithdrawalLimitExceeded());
        window.used += p.amount;
        balances[p.account][p.token] -= debit;
        paymentNonce[p.account]++;
        IERC20 token = IERC20(p.token);
        uint256 beforeBalance = token.balanceOf(address(this));
        uint256 recipientBefore = token.balanceOf(p.recipient);
        uint256 accountBefore = token.balanceOf(p.account);
        token.safeTransfer(p.recipient, p.amount);
        token.safeTransfer(p.account, p.amount);
        require(token.balanceOf(address(this)) == beforeBalance - debit, NonExactTransfer());
        require(
            token.balanceOf(p.account) == accountBefore + (p.recipient == p.account ? debit : p.amount),
            NonExactTransfer()
        );
        require(
            token.balanceOf(p.recipient) == recipientBefore + (p.recipient == p.account ? debit : p.amount),
            NonExactTransfer()
        );
        emit BundlePayment(p.account, p.token, p.recipient, p.amount);
    }

    function paymentDigest(Payment calldata p) public view override returns (bytes32) {
        return _hashTypedDataV4(
            keccak256(abi.encode(PAYMENT_TYPEHASH, p.account, p.token, p.recipient, p.amount, p.nonce, p.deadline))
        );
    }

    function balanceOf(address account, address token) external view override returns (uint256) {
        return balances[account][token];
    }

    function bundleTokensOf(address account) external view override returns (address[] memory) {
        return tokens[account];
    }

    function bundleSendersOf(address account) external view override returns (address[] memory) {
        return senders[account];
    }

    function isBundleToken(address account, address token) public view override returns (bool) {
        for (uint256 i; i < tokens[account].length; ++i) {
            if (tokens[account][i] == token) return true;
        }
        return false;
    }

    function _allowed(address account, address sender) private view returns (bool) {
        for (uint256 i; i < senders[account].length; ++i) {
            if (senders[account][i] == sender) return true;
        }
        return false;
    }

    function _requireEmpty(address account) private view {
        for (uint256 i; i < tokens[account].length; ++i) {
            address token = tokens[account][i];
            require(balances[account][token] == 0, HasBundleBalance(token));
        }
    }

    function _active(address cca) private view {
        ICca.State state = ICca(CCA_REGISTRY).getCcaState(cca);
        require(cca != address(0) && state == ICca.State.Active, CcaNotActive(cca, state));
    }
}
