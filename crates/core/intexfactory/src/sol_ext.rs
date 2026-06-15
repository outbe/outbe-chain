//! External contract ABIs invoked via `storage.call` / `staticcall`.

use alloy_sol_types::sol;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    struct MessagingFee {
        uint256 nativeFee;
        uint256 lzTokenFee;
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IOriginMessenger {
        struct IssuanceInstructionsParams {
            uint32 seriesId;
            uint32 issuedIntexCount;
            uint128 promisLoadMinor;
            uint64 costAmountMinor;
            uint64 floorPriceMinor;
            uint32 intexCallPeriod;
            uint16 settlementTokenAlias;
            uint16 callWindowDays;
            uint16 callThresholdDays;
            uint64 coenPriceCallTrigger;
            address[] recipients;
            uint256[] quantities;
        }

        function quoteSendIssuanceInstructions(
            IssuanceInstructionsParams calldata params,
            bytes calldata extraOptions,
            bool payInLzToken
        ) external view returns (MessagingFee memory fee);

        function sendIssuanceInstructions(
            IssuanceInstructionsParams calldata params,
            bytes calldata extraOptions,
            MessagingFee calldata fee,
            address refundAddress
        ) external payable returns (bytes32 guid);

        function quoteSendMarkQualified(
            uint32 seriesId,
            bytes calldata extraOptions,
            bool payInLzToken
        ) external view returns (MessagingFee memory fee);

        function sendMarkQualified(
            uint32 seriesId,
            bytes calldata extraOptions,
            MessagingFee calldata fee,
            address refundAddress
        ) external payable returns (bytes32 guid);

        function quoteSendMarkCalled(
            uint32 seriesId,
            bytes calldata extraOptions,
            bool payInLzToken
        ) external view returns (MessagingFee memory fee);

        function sendMarkCalled(
            uint32 seriesId,
            bytes calldata extraOptions,
            MessagingFee calldata fee,
            address refundAddress
        ) external payable returns (bytes32 guid);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IERC20 {
        function transferFrom(address from, address to, uint256 amount) external returns (bool);
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IVaultProvider {
        function assetAt(uint256 index) external view returns (address);
        function depositLiquidity(address asset, uint256 assetsAmount)
            external returns (uint256 sharesAmount);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IIntexNFT1155 {
        function createSeries(uint32 seriesId, uint32 issuedIntexCount, uint32 intexCallPeriod) external;
        function balanceOf(address account, uint256 id) external view returns (uint256);
        function settle(uint32 seriesId, address from, address to, uint256 amount) external;
        function burnSettled(address holder, uint32 seriesId, uint256 amount) external;
    }
}
