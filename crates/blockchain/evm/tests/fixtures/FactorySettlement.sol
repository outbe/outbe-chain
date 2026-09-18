// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// Stateful token, vault and IntexNFT1155 counterparties for intex_gem_erc20_settlement.rs.
/// Compile with solc 0.8.30 --optimize --evm-version prague --bin-runtime --metadata-hash none.
contract FactorySettlement {
    address constant ASSET = 0x3333333333333333333333333333333333333333;
    address constant ROUTER = address(0x1017);
    mapping(address => uint256) internal balances;
    mapping(address => mapping(address => uint256)) public allowance;
    mapping(address => mapping(uint256 => uint256)) internal units;
    uint256 public mode;
    address public factory;
    bytes public reentry;
    bool public callbackRejected;

    function configure(uint256 mode_, address factory_, bytes calldata reentry_) external {
        mode = mode_;
        factory = factory_;
        reentry = reentry_;
    }

    function mint(address account, uint256 amount) external {
        balances[account] += amount;
    }

    function asset() external pure returns (address) { return ASSET; }

    function isoCode() external pure returns (uint16) { return 840; }

    function decimals() external pure returns (uint8) { return 6; }

    function approve(address spender, uint256 amount) external returns (bool) {
        if (mode == 2 && msg.sender == factory) return false;
        allowance[msg.sender][spender] = amount;
        if (mode == 7) assembly { return(0, 0) }
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        if ((mode == 1 && msg.sender == factory) || (mode == 3 && msg.sender == ROUTER)) return false;
        if (mode == 6) assembly { mstore(0, 2) return(0, 32) }
        if (mode == 8) return true;
        if (mode == 5 && msg.sender == factory) {
            (bool ok,) = factory.call(reentry);
            require(!ok, "REENTERED_SETTLEMENT");
            callbackRejected = true;
        }
        allowance[from][msg.sender] -= amount;
        balances[from] -= amount;
        balances[to] += mode == 9 ? amount - 1 : amount;
        if (mode == 7) assembly { return(0, 0) }
        return true;
    }

    function deposit(uint256 amount, address onBehalf) external returns (uint256) {
        require(mode != 4, "VAULT_FAILURE");
        (bool ok, bytes memory ret) = ASSET.call(abi.encodeWithSignature(
            "transferFrom(address,address,uint256)", msg.sender, address(this), amount
        ));
        require(ok && (ret.length == 0 || abi.decode(ret, (bool))), "TRANSFER_FAILED");
        balances[onBehalf] += amount;
        return amount;
    }

    function mint1155(address account, uint256 id, uint256 amount) external {
        units[account][id] += amount;
    }

    function balanceOf(address account) external view returns (uint256) {
        return balances[account];
    }

    function balanceOf(address account, uint256 id) external view returns (uint256) {
        return units[account][id];
    }

    function settleIntex(bytes14 seriesId, address from, address to, uint256 amount) external {
        uint256 id = uint256(uint112(seriesId));
        units[from][id] -= amount;
        units[to][id | (1 << 112)] += amount;
    }
}
