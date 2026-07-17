//! External contract ABIs invoked via `storage.call` / `staticcall`.

use alloy_sol_types::sol;

sol! {
    // `OriginRouter` sends are relay-float-funded: called with value 0, the router quotes and pays
    // the bridge fee from its own native balance, so the precompile passes no fee/options/refund.
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IOriginRouter {
        struct IssuanceInstructionsParams {
            uint32 seriesId;
            uint32 worldwideDay;
            uint32 issuedIntexCount;
            uint128 promisLoadMinor;
            uint64 entryPriceMinor;
            uint64 floorPriceMinor;
            uint32 intexCallPeriod;
            uint16 issuanceCurrency;
            uint16 referenceCurrency;
            uint16 callWindowDays;
            uint16 callThresholdDays;
            uint64 callPriceMinor;
            address[] recipients;
            uint256[] quantities;
        }

        function sendIssuanceInstructions(IssuanceInstructionsParams calldata params)
            external payable returns (bytes32 sendId);

        function sendMarkQualified(uint32 seriesId) external payable returns (bytes32 sendId);

        function sendMarkCalled(uint32 seriesId) external payable returns (bytes32 sendId);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IERC20 {
        function transferFrom(address from, address to, uint256 amount) external returns (bool);
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IIntexNFT1155 {
        struct IntexCallTrigger {
            uint16 windowDays;
            uint16 thresholdDays;
            uint32 intexCallPeriod;
        }

        struct CreateSeriesParams {
            uint32 seriesId;
            uint32 worldwideDay;
            uint16 issuanceCurrency;
            uint16 referenceCurrency;
            uint32 issuedIntexCount;
            uint128 promisLoadMinor;
            uint64 entryPriceMinor;
            uint64 floorPriceMinor;
            uint64 callPriceMinor;
            IntexCallTrigger callTrigger;
        }

        function createSeries(CreateSeriesParams params) external;
        function balanceOf(address account, uint256 id) external view returns (uint256);
        function settle(uint32 seriesId, address from, address to, uint256 amount) external;
        function burnSettled(address holder, uint32 seriesId, uint256 amount) external;
        function markQualified(uint32 seriesId) external;
        function markCalled(uint32 seriesId) external;
    }
}
