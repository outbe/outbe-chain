// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity 0.8.30;

import {Script, console} from "forge-std/Script.sol";

contract BaseScript is Script {
    // Salt configuration - change this to deploy to different addresses
    // Using the same salt across different chains will result in the same addresses
    // To deploy to different addresses, use: SALT_VERSION = "v1.0.1", "v2.0.0", etc.
    string public constant SALT_VERSION = "v0.0.1";

    // Note: CREATE2_FACTORY (0x4e59b44847b379578588920cA78FbF26c0B4956C) is inherited from Script
    // and used when always_use_create_2_factory = true in foundry.toml

    address internal owner;
    uint256 internal privateKey;

    constructor() {}

    function setUp() public {
        privateKey = deployerPrivateKey();
        address signer = vm.addr(privateKey);
        owner = vm.envOr("OWNER_ADDRESS", signer);
    }

    /// @notice Read PRIVATE_KEY from environment, accepting both "0x"-prefixed and raw hex strings
    function deployerPrivateKey() internal view returns (uint256) {
        string memory raw = vm.envString("PRIVATE_KEY");
        if (bytes(raw).length >= 2 && bytes(raw)[0] == "0" && (bytes(raw)[1] == "x" || bytes(raw)[1] == "X")) {
            return vm.parseUint(raw);
        }
        return vm.parseUint(string.concat("0x", raw));
    }

    /// @notice Generate export string for deployment output
    /// @param name Variable name (e.g., "VAULT_ADDRESS")
    /// @param value Variable value (e.g., "0x123...")
    /// @return Complete export statement (e.g., "export VAULT_ADDRESS=0x123...")
    function exportLine(string memory name, string memory value) public pure returns (string memory) {
        return string.concat("export ", name, "=", value);
    }

    /// @notice Get environment name based on chain ID
    /// @return Environment name string
    function getEnvName() public view returns (string memory) {
        uint256 chainId = block.chainid;

        if (chainId == 31337) return "anvil";
        if (chainId == 424242) return "outbe-dev";
        if (chainId == 424243) return "local-dev";
        if (chainId == 97) return "bsc-testnet";
        if (chainId == 1) return "mainnet";
        if (chainId == 11155111) return "sepolia";
        if (chainId == 137) return "polygon";
        if (chainId == 42161) return "arbitrum";
        if (chainId == 10) return "optimism";
        if (chainId == 8453) return "base";
        if (chainId == 512512) return "outbe-privnet";
        if (chainId == 512215) return "local-reth";
        if (chainId == 54322345) return "outbe-peira";

        return string.concat("chain-", vm.toString(chainId));
    }

    /// @notice Get deployment output file path based on current chain
    /// @return File name (e.g., ".anvil.deployment.env")
    function deploymentFile() public view returns (string memory) {
        return string.concat(".", getEnvName(), ".deployment.env");
    }

    /// @notice Resolve the absolute path for deployment output.
    /// @dev If `DEPLOYMENT_ENV_FILE` env var is set (typically by deploy.sh),
    ///      it overrides the chain-id-derived default. This lets the shell
    ///      caller pin the file name so both writer and sourcer agree.
    function deploymentFilePath() public view returns (string memory) {
        string memory envFile = vm.envOr("DEPLOYMENT_ENV_FILE", string(""));
        if (bytes(envFile).length > 0) {
            return envFile;
        }
        return string.concat(vm.projectRoot(), "/", deploymentFile());
    }

    function writeToDeploymentFile(string memory data) public {
        // forge-lint: disable-next-line(unsafe-cheatcode)
        vm.writeLine(deploymentFilePath(), data);
    }

    function printAndWrite(string memory data) public {
        console.log(data);
        writeToDeploymentFile(data);
    }

    /// @notice Generate deterministic salt for Create2 deployment
    /// @param name Name of the contract (e.g., "Vault", "VaultProvider")
    /// @return Keccak256 hash of name + chain ID + salt version
    function generateSalt(string memory name) internal view returns (bytes32) {
        return keccak256(abi.encodePacked(name, block.chainid, SALT_VERSION));
    }
}
