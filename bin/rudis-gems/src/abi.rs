// The canonical Solidity ABI generates constructors with more than seven parameters.
#![allow(clippy::too_many_arguments)]

use alloy_sol_types::sol;

sol!("../../contracts/precompiles/src/IGem.sol");
sol!("../../contracts/precompiles/src/IGemFactory.sol");
sol!("../../contracts/precompiles/src/IPromis.sol");
sol!("../../contracts/precompiles/src/IPayNote.sol");
sol!("../../contracts/tokens/src/interfaces/IERC20.sol");

sol! {
    interface IRudisFactory {
        function mineRudis(uint256 amount, bytes32 mac, uint64 opNonce) external returns (uint256);
        event RudisMined(address indexed sender, uint256 amount);
    }
}
