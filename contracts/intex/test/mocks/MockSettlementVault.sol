// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

contract MockSettlementVault {
    IERC20 public immutable ASSET_TOKEN;

    string public name;
    string public symbol;
    uint8 public immutable DECIMALS;

    uint256 public totalSupply;
    uint256 public assetsPerShare = 1e18;

    mapping(address => uint256) private _balances;
    mapping(address => mapping(address => uint256)) private _allowances;

    constructor(address _assetToken, string memory _name, string memory _symbol, uint8 _decimals) {
        ASSET_TOKEN = IERC20(_assetToken);
        name = _name;
        symbol = _symbol;
        DECIMALS = _decimals;
    }

    function asset() external view returns (address) {
        return address(ASSET_TOKEN);
    }

    function decimals() external view returns (uint8) {
        return DECIMALS;
    }

    function balanceOf(address account) external view returns (uint256) {
        return _balances[account];
    }

    function allowance(address owner, address spender) external view returns (uint256) {
        return _allowances[owner][spender];
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        _allowances[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _balances[msg.sender] -= amount;
        _balances[to] += amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = _allowances[from][msg.sender];
        if (msg.sender != from && allowed != type(uint256).max) {
            _allowances[from][msg.sender] = allowed - amount;
        }
        _balances[from] -= amount;
        _balances[to] += amount;
        return true;
    }

    function previewWithdraw(uint256 assets) public view returns (uint256 shares) {
        return _ceilDiv(assets * 1e18, assetsPerShare);
    }

    function previewDeposit(uint256 assets) public view returns (uint256 shares) {
        return (assets * 1e18) / assetsPerShare;
    }

    function deposit(uint256 assets, address onBehalf) external returns (uint256 shares) {
        shares = previewDeposit(assets);
        require(ASSET_TOKEN.transferFrom(msg.sender, address(this), assets), "TRANSFER_FROM_FAILED");
        _balances[onBehalf] += shares;
        totalSupply += shares;
    }

    function withdraw(uint256 assets, address receiver, address onBehalf) external returns (uint256 shares) {
        shares = previewWithdraw(assets);

        if (msg.sender != onBehalf) {
            uint256 allowed = _allowances[onBehalf][msg.sender];
            if (allowed != type(uint256).max) {
                _allowances[onBehalf][msg.sender] = allowed - shares;
            }
        }

        _balances[onBehalf] -= shares;
        totalSupply -= shares;
        require(ASSET_TOKEN.transfer(receiver, assets), "TRANSFER_FAILED");
    }

    function setAssetsPerShare(uint256 newAssetsPerShare) external {
        require(newAssetsPerShare != 0, "ZERO_RATE");
        assetsPerShare = newAssetsPerShare;
    }

    function _ceilDiv(uint256 a, uint256 b) private pure returns (uint256) {
        return a == 0 ? 0 : ((a - 1) / b) + 1;
    }
}
