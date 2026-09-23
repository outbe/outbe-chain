// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// Stateful token/vault counterparty for credis_issuance.rs; compile with solc 0.8.33,
/// optimizer 200, via-ir, Prague, metadata hash none, CBOR disabled.
contract CredisIssuance {
    address constant ASSET = 0x3333333333333333333333333333333333333333;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    uint256 public mode;
    function configure(uint256 value) external { mode = value; }
    function mint(address account, uint256 amount) external { balanceOf[account] += amount; }
    function isoCode() external pure returns (uint16) { return 840; }
    function approve(address spender, uint256 amount) external returns (bool) {
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
