// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// Stateful token/vault counterparty for credis_issuance.rs; compile with solc 0.8.33,
/// optimizer 200, via-ir, Prague, metadata hash none, CBOR disabled.
contract CredisIssuance {
    address constant ASSET = 0x3333333333333333333333333333333333333333;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    uint256 public mode;
    uint256 public position;
    function setPosition(uint256 id) external { position = id; }
    function configure(uint256 value) external { mode = value; }
    function mint(address account, uint256 amount) external { balanceOf[account] += amount; }
    function isoCode() external view returns (uint16) { return mode == 6 ? 978 : 840; }
    function decimals() external view returns (uint8) { return mode == 7 ? 18 : 6; }
    function approve(address spender, uint256 amount) external returns (bool) {
        if (mode == 10) return false;
        allowance[msg.sender][spender] = amount; return true;
    }
    function transfer(address to, uint256 amount) external returns (bool) {
        if (mode == 1) return false;
        require(mode != 2, "PAYOUT_FAILED");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }
    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        if (mode == 1) return false;
        require(mode != 2, "PAYMENT_FAILED");
        if (mode == 9) {
            mode = 0;
            balanceOf[address(this)] += 1;
            allowance[address(this)][address(0x1009)] = 1;
            (bool ok, bytes memory reason) = address(0x1009).call(abi.encodeWithSignature("settleCredis(uint256,uint256)", position, 1));
            if (!ok) { assembly { revert(add(reason, 32), mload(reason)) } }
        }
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        return true;
    }
    function deposit(uint256 amount, address onBehalf) external returns (uint256) {
        require(mode != 3, "VAULT_FAILED");
        require(CredisIssuance(ASSET).transferFrom(msg.sender, address(this), amount));
        balanceOf[onBehalf] += amount;
        return amount;
    }
}
