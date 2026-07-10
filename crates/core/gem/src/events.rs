use alloy_sol_types::sol;

sol! {
    #[derive(Debug, PartialEq)]
    event GemQualified(uint256 indexed gemId, uint64 qualifiedAt);

    #[derive(Debug, PartialEq)]
    event GemCalled(uint256 indexed gemId, uint32 calledAt);

    /// Emitted when a Called gem is forfeit-burned after its notice period
    /// lapsed. Same signature as the gemfactory mining burn.
    #[derive(Debug, PartialEq)]
    event GemBurned(uint256 indexed gemId, address owner, uint256 gemLoad);
}
