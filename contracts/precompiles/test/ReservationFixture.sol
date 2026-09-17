// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

// Stateful fixtures for the Rust EVM reservation tests. Runtime bytecode is
// checked in alongside those tests so Cargo does not need a Solidity compiler.
contract ReservationToken {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    bool public fee;

    function isoCode() external pure returns (uint16) {
        return 840;
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function setFee(bool enabled) external {
        fee = enabled;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        move(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        allowance[from][msg.sender] -= amount;
        move(from, to, amount);
        return true;
    }

    function move(address from, address to, uint256 amount) internal {
        balanceOf[from] -= amount;
        balanceOf[to] += fee && amount > 0 ? amount - 1 : amount;
    }
}

contract ReservationVault {
    ReservationToken public asset;
    mapping(address => uint256) public balanceOf;
    bool public rejectDeposits;

    function configure(ReservationToken token, address router, uint256 shares) external {
        asset = token;
        balanceOf[router] = shares;
    }

    function setRejectDeposits(bool reject) external {
        rejectDeposits = reject;
    }

    function previewWithdraw(uint256 amount) external pure returns (uint256) {
        return amount;
    }

    function withdraw(uint256 amount, address receiver, address owner) external returns (uint256) {
        require(msg.sender == owner);
        balanceOf[owner] -= amount;
        require(asset.transfer(receiver, amount));
        return amount;
    }

    function deposit(uint256 amount, address receiver) external returns (uint256) {
        require(!rejectDeposits);
        require(asset.transferFrom(msg.sender, address(this), amount));
        balanceOf[receiver] += amount;
        return amount;
    }
}

contract ReservationReceiver {
    function topUp(address from, address token, uint256 amount) external {
        require(ReservationToken(token).transferFrom(from, address(this), amount));
    }
}
