use alloy_sol_types::sol;

sol! {
    #[derive(Debug, PartialEq)]
    event GemIssued(
        uint256 indexed gemId,
        uint8 gemType,
        address owner,
        uint256 gemLoad,
        uint256 entryPrice,
        uint256 costAmount,
        uint256 floorPrice,
        uint64 issuedAt
    );

    #[derive(Debug, PartialEq)]
    event GemSettled(
        uint256 indexed gemId,
        address owner,
        uint256 amountPaid,
        uint16 issuanceCurrency
    );

    #[derive(Debug, PartialEq)]
    event GemBurned(
        uint256 indexed gemId,
        address owner,
        uint256 gemLoad
    );
}
