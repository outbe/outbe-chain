// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {BnbVaultProvider} from "src/BnbVaultProvider.sol";
import {IERC7786GatewaySource, IERC7786Recipient, IGatewayQuote} from "src/interfaces/IERC7786.sol";
import {IERC7786TokenReceiver} from "src/interfaces/IERC7786TokenReceiver.sol";

contract MockWCOEN is ERC20 {
    constructor() ERC20("Wrapped COEN", "WCOEN") {}

    function mint(address account, uint256 amount) external {
        _mint(account, amount);
    }
}

contract MockOneToOneVault is ERC20 {
    using SafeERC20 for IERC20;

    IERC20 public immutable underlying;
    uint256 public shareDelta;

    constructor(IERC20 asset_) ERC20("WCOEN Vault Share", "vWCOEN") {
        underlying = asset_;
    }

    function setShareDelta(uint256 delta) external {
        shareDelta = delta;
    }

    function asset() external view returns (address) {
        return address(underlying);
    }

    function deposit(uint256 assets, address onBehalf) external returns (uint256 shares) {
        underlying.safeTransferFrom(msg.sender, address(this), assets);
        shares = assets + shareDelta;
        _mint(onBehalf, shares);
    }

    function withdraw(uint256 assets, address receiver, address onBehalf) external returns (uint256 burnedShares) {
        burnedShares = assets + shareDelta;
        _burn(onBehalf, burnedShares);
        underlying.safeTransfer(receiver, assets);
    }
}

contract MockMessageBridge is IERC7786GatewaySource, IGatewayQuote {
    uint256 public fee;
    bytes public lastRecipient;
    bytes public lastPayload;
    uint256 public lastValue;
    bytes32 public lastSendId;

    function setFee(uint256 fee_) external {
        fee = fee_;
    }

    function supportsAttribute(bytes4) external pure returns (bool) {
        return true;
    }

    function quote(bytes calldata, bytes calldata) external view returns (uint256) {
        return fee;
    }

    function quote(bytes calldata, bytes calldata, bytes[] calldata) external view returns (uint256) {
        return fee;
    }

    function sendMessage(bytes calldata recipient, bytes calldata payload, bytes[] calldata)
        external
        payable
        returns (bytes32 sendId)
    {
        require(msg.value == fee, "wrong fee");
        lastRecipient = recipient;
        lastPayload = payload;
        lastValue = msg.value;
        sendId = keccak256(abi.encode(msg.sender, recipient, payload));
        lastSendId = sendId;
        emit MessageSent(sendId, "", recipient, payload, msg.value, new bytes[](0));
    }

    function deliver(BnbVaultProvider recipient, bytes32 receiveId, bytes calldata sender, bytes calldata payload)
        external
        returns (bytes4)
    {
        return recipient.receiveMessage(receiveId, sender, payload);
    }
}

contract MockTokenBridge {
    using SafeERC20 for IERC20;

    MockWCOEN public immutable token;
    uint256 public fee;
    uint32 public lastDestination;
    address public lastReceiver;
    uint256 public lastAmount;
    bytes public lastExtraData;
    uint256 public lastGasLimit;

    constructor(MockWCOEN token_) {
        token = token_;
    }

    function setFee(uint256 fee_) external {
        fee = fee_;
    }

    function quoteSend(uint32, address, uint256, bytes calldata, uint256) external view returns (uint256) {
        return fee;
    }

    function sendAndCall(
        uint32 destinationDomain,
        address to,
        uint256 amount,
        bytes calldata extraData,
        uint256 gasLimit
    ) external payable returns (bytes32 sendId) {
        require(msg.value == fee, "wrong fee");
        IERC20(address(token)).safeTransferFrom(msg.sender, address(this), amount);
        lastDestination = destinationDomain;
        lastReceiver = to;
        lastAmount = amount;
        lastExtraData = extraData;
        lastGasLimit = gasLimit;
        return keccak256(abi.encode(destinationDomain, to, amount, extraData));
    }

    function deliverDeposit(
        BnbVaultProvider recipient,
        uint32 sourceDomain,
        bytes calldata from,
        uint256 amount,
        bytes calldata extraData
    ) external returns (bytes4) {
        token.mint(address(recipient), amount);
        return recipient.onCrosschainTokensReceived(sourceDomain, from, amount, extraData);
    }
}

contract BnbVaultProviderTest is Test {
    uint32 internal constant OUTBE_DOMAIN = 54_322_345;
    address internal constant OUTBE_PROVIDER = 0x0000000000000000000000000000000000001017;
    uint256 internal constant ACK_GAS_LIMIT = 300_000;
    uint256 internal constant RETURN_GAS_LIMIT = 400_000;

    MockWCOEN internal asset;
    MockOneToOneVault internal vault;
    MockMessageBridge internal messageBridge;
    MockTokenBridge internal tokenBridge;
    BnbVaultProvider internal provider;
    address internal user;
    bytes internal outbeSender;

    function setUp() external {
        asset = new MockWCOEN();
        vault = new MockOneToOneVault(asset);
        messageBridge = new MockMessageBridge();
        tokenBridge = new MockTokenBridge(asset);
        provider = new BnbVaultProvider(
            address(asset),
            address(vault),
            address(tokenBridge),
            address(messageBridge),
            OUTBE_DOMAIN,
            OUTBE_PROVIDER,
            address(this)
        );
        user = makeAddr("user");
        outbeSender = InteroperableAddress.formatEvmV1(OUTBE_DOMAIN, OUTBE_PROVIDER);
    }

    function test_crosschainDeposit_depositsMintedWCOENAndSendsAcknowledgement() external {
        uint256 amount = 100 ether;
        bytes32 operationId = keccak256("deposit-1");
        uint256 acknowledgementFee = 0.01 ether;
        messageBridge.setFee(acknowledgementFee);
        vm.deal(address(provider), acknowledgementFee);

        bytes4 result = tokenBridge.deliverDeposit(
            provider, OUTBE_DOMAIN, outbeSender, amount, _depositData(operationId, user, amount)
        );

        assertEq(result, IERC7786TokenReceiver.onCrosschainTokensReceived.selector);
        assertEq(asset.balanceOf(address(vault)), amount);
        assertEq(vault.balanceOf(address(provider)), amount);
        assertEq(provider.totalManagedShares(), amount);
        assertEq(messageBridge.lastValue(), acknowledgementFee);
        assertEq(messageBridge.lastRecipient(), outbeSender);

        (uint256 kind, bytes32 ackOperationId, address ackUser, uint256 ackAmount) =
            abi.decode(messageBridge.lastPayload(), (uint256, bytes32, address, uint256));
        assertEq(kind, provider.DEPOSIT_ACKNOWLEDGEMENT());
        assertEq(ackOperationId, operationId);
        assertEq(ackUser, user);
        assertEq(ackAmount, amount);
    }

    function test_crosschainWithdraw_burnsOneToOneSharesAndBridgesWCOENBack() external {
        uint256 deposited = 100 ether;
        _deposit(keccak256("deposit"), deposited);

        uint256 amount = 40 ether;
        bytes32 operationId = keccak256("withdraw");
        uint256 returnFee = 0.02 ether;
        tokenBridge.setFee(returnFee);
        vm.deal(address(provider), returnFee);

        bytes memory payload = abi.encode(provider.WITHDRAW_REQUEST(), operationId, user, amount, RETURN_GAS_LIMIT);
        bytes4 result = messageBridge.deliver(provider, bytes32(uint256(1)), outbeSender, payload);

        assertEq(result, IERC7786Recipient.receiveMessage.selector);
        assertEq(vault.balanceOf(address(provider)), deposited - amount);
        assertEq(provider.totalManagedShares(), deposited - amount);
        assertEq(tokenBridge.lastDestination(), OUTBE_DOMAIN);
        assertEq(tokenBridge.lastReceiver(), OUTBE_PROVIDER);
        assertEq(tokenBridge.lastAmount(), amount);
        assertEq(tokenBridge.lastGasLimit(), RETURN_GAS_LIMIT);

        (uint256 kind, bytes32 returnedOperationId, address returnedUser, uint256 returnedAmount) =
            abi.decode(tokenBridge.lastExtraData(), (uint256, bytes32, address, uint256));
        assertEq(kind, provider.WITHDRAW_RETURN());
        assertEq(returnedOperationId, operationId);
        assertEq(returnedUser, user);
        assertEq(returnedAmount, amount);
    }

    function test_depositRevertsAndRollsBackWhenVaultBreaksOneToOneInvariant() external {
        vault.setShareDelta(1);
        uint256 amount = 100 ether;
        bytes32 operationId = keccak256("bad-shares");

        vm.expectRevert(abi.encodeWithSelector(BnbVaultProvider.InvalidShareAmount.selector, amount, amount + 1));
        tokenBridge.deliverDeposit(provider, OUTBE_DOMAIN, outbeSender, amount, _depositData(operationId, user, amount));

        assertEq(asset.balanceOf(address(provider)), 0);
        assertEq(asset.balanceOf(address(vault)), 0);
        assertEq(vault.balanceOf(address(provider)), 0);
        (BnbVaultProvider.OperationKind kind,,) = provider.operations(operationId);
        assertEq(uint256(kind), uint256(BnbVaultProvider.OperationKind.None));
    }

    function test_depositRequiresGasTankAndRemainsRetryable() external {
        messageBridge.setFee(1 ether);
        uint256 amount = 10 ether;
        bytes32 operationId = keccak256("needs-gas");

        vm.expectRevert(abi.encodeWithSelector(BnbVaultProvider.InsufficientNativeGas.selector, uint256(0), 1 ether));
        tokenBridge.deliverDeposit(provider, OUTBE_DOMAIN, outbeSender, amount, _depositData(operationId, user, amount));

        vm.deal(address(provider), 1 ether);
        tokenBridge.deliverDeposit(provider, OUTBE_DOMAIN, outbeSender, amount, _depositData(operationId, user, amount));
        assertEq(provider.totalManagedShares(), amount);
    }

    function test_withdrawRevertsAndRollsBackWhenVaultBreaksOneToOneInvariant() external {
        uint256 deposited = 100 ether;
        _deposit(keccak256("deposit-before-bad-withdraw"), deposited);
        vault.setShareDelta(1);

        uint256 amount = 40 ether;
        bytes32 operationId = keccak256("bad-withdraw-shares");
        bytes memory payload = abi.encode(provider.WITHDRAW_REQUEST(), operationId, user, amount, RETURN_GAS_LIMIT);

        vm.expectRevert(abi.encodeWithSelector(BnbVaultProvider.InvalidShareAmount.selector, amount, amount + 1));
        messageBridge.deliver(provider, bytes32(uint256(1)), outbeSender, payload);

        assertEq(vault.balanceOf(address(provider)), deposited);
        assertEq(provider.totalManagedShares(), deposited);
        (BnbVaultProvider.OperationKind kind,,) = provider.operations(operationId);
        assertEq(uint256(kind), uint256(BnbVaultProvider.OperationKind.None));
    }

    function test_withdrawRequiresGasTankAndRemainsRetryable() external {
        uint256 deposited = 100 ether;
        _deposit(keccak256("deposit-before-gas-retry"), deposited);

        uint256 amount = 40 ether;
        uint256 returnFee = 1 ether;
        bytes32 operationId = keccak256("withdraw-needs-gas");
        bytes memory payload = abi.encode(provider.WITHDRAW_REQUEST(), operationId, user, amount, RETURN_GAS_LIMIT);
        tokenBridge.setFee(returnFee);

        vm.expectRevert(abi.encodeWithSelector(BnbVaultProvider.InsufficientNativeGas.selector, uint256(0), returnFee));
        messageBridge.deliver(provider, bytes32(uint256(1)), outbeSender, payload);
        assertEq(vault.balanceOf(address(provider)), deposited);
        assertEq(provider.totalManagedShares(), deposited);

        vm.deal(address(provider), returnFee);
        messageBridge.deliver(provider, bytes32(uint256(2)), outbeSender, payload);
        assertEq(vault.balanceOf(address(provider)), deposited - amount);
        assertEq(provider.totalManagedShares(), deposited - amount);
    }

    function test_replayAndAuthenticationAreRejected() external {
        uint256 amount = 10 ether;
        bytes32 operationId = keccak256("replay");
        _deposit(operationId, amount);

        vm.expectRevert(abi.encodeWithSelector(BnbVaultProvider.OperationAlreadyExecuted.selector, operationId));
        tokenBridge.deliverDeposit(provider, OUTBE_DOMAIN, outbeSender, amount, _depositData(operationId, user, amount));

        vm.expectRevert(BnbVaultProvider.UnauthorizedCrosschainSender.selector);
        tokenBridge.deliverDeposit(
            provider,
            OUTBE_DOMAIN,
            InteroperableAddress.formatEvmV1(OUTBE_DOMAIN, makeAddr("attacker")),
            amount,
            _depositData(keccak256("attacker"), user, amount)
        );
    }

    function _deposit(bytes32 operationId, uint256 amount) internal {
        tokenBridge.deliverDeposit(provider, OUTBE_DOMAIN, outbeSender, amount, _depositData(operationId, user, amount));
    }

    function _depositData(bytes32 operationId, address account, uint256 amount) internal pure returns (bytes memory) {
        return abi.encode(uint256(1), operationId, account, amount, ACK_GAS_LIMIT);
    }
}
