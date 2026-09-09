// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITokenBundle {
    enum Status {
        Unopened,
        Open,
        Closed
    }

    struct Payment {
        address account;
        address token;
        address recipient;
        uint256 amount;
        uint256 nonce;
        uint256 deadline;
    }

    function topUpFor(address account, address token, uint256 amount) external;
    function spend(Payment calldata payment, bytes calldata signature) external;
    function paymentDigest(Payment calldata payment) external view returns (bytes32);
    function paymentNonce(address account) external view returns (uint256);
    function balanceOf(address account, address token) external view returns (uint256);
    function bundleTokensOf(address account) external view returns (address[] memory);
    function bundleSendersOf(address account) external view returns (address[] memory);
    function status(address account) external view returns (Status);
    function linkedCca(address account) external view returns (address);
    function isBundleToken(address account, address token) external view returns (bool);
}
