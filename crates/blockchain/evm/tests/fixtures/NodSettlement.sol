// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// Stateful token and vault counterparties for nod_erc20_settlement.rs.
/// Compile with solc 0.8.30 --optimize --evm-version prague --bin-runtime --metadata-hash none.
contract NodSettlement {
    address constant ASSET = 0x3333333333333333333333333333333333333333;
    address constant FACTORY = address(0x1007);
    address constant ROUTER = address(0x1017);
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    uint256 public mode;
    uint256 public nodId;
    bool public callbackRejected;

    function configure(uint256 mode_, uint256 nodId_) external {
        mode = mode_;
        nodId = nodId_;
    }

    function mint(address account, uint256 amount) external {
        balanceOf[account] += amount;
    }

    function asset() external pure returns (address) { return ASSET; }

    function isoCode() external pure returns (uint16) { return 840; }
    function decimals() external pure returns (uint8) { return 6; }

    function approve(address spender, uint256 amount) external returns (bool) {
        if (mode == 2 && msg.sender == FACTORY) return false;
        allowance[msg.sender][spender] = amount;
        if (mode == 7) assembly { return(0, 0) }
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        if ((mode == 1 && msg.sender == FACTORY) || (mode == 3 && msg.sender == ROUTER)) return false;
        if (mode == 6) assembly { mstore(0, 2) return(0, 32) }
        if (mode == 8) return true;
        if (mode == 5 && msg.sender == FACTORY) {
            (bool ok,) = FACTORY.call(abi.encodeWithSignature("settleNod(uint256,address)", nodId, ASSET));
            require(!ok, "REENTERED_SETTLEMENT");
            callbackRejected = true;
        }
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += mode == 9 ? amount - 1 : amount;
        if (mode == 7) assembly { return(0, 0) }
        return true;
    }

    function deposit(uint256 amount, address onBehalf) external returns (uint256) {
        require(mode != 4, "VAULT_FAILURE");
        (bool ok, bytes memory ret) = ASSET.call(abi.encodeWithSignature(
            "transferFrom(address,address,uint256)", msg.sender, address(this), amount
        ));
        require(ok && (ret.length == 0 || abi.decode(ret, (bool))), "TRANSFER_FAILED");
        balanceOf[onBehalf] += amount;
        return amount;
    }
}
