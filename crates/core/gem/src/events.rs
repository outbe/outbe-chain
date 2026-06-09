use alloy_sol_types::sol;

sol! {
    #[derive(Debug, PartialEq)]
    event GemQualified(uint256 indexed gemId, uint64 qualifiedAt);
}
