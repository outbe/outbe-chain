// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity 0.8.30;

import {BaseScript} from "./BaseScript.s.sol";
import {IVaultV2} from "../src/interfaces/IVaultV2.sol";

contract SetVaultGates is BaseScript {
    function run() external returns (address vault) {
        address vaultProvider = vm.envAddress("VAULT_PROVIDER_ADDRESS");
        vault = vm.envAddress("VAULT_ADDRESS");

        require(vaultProvider != address(0), "VAULT_PROVIDER_REQUIRED");
        require(vault != address(0), "VAULT_REQUIRED");

        IVaultV2 vaultContract = IVaultV2(vault);

        vm.startBroadcast(privateKey);

        bytes memory receiveSharesGateCall = abi.encodeCall(IVaultV2.setReceiveSharesGate, (vaultProvider));
        bytes memory sendSharesGateCall = abi.encodeCall(IVaultV2.setSendSharesGate, (vaultProvider));
        bytes memory receiveAssetsGateCall = abi.encodeCall(IVaultV2.setReceiveAssetsGate, (vaultProvider));
        bytes memory sendAssetsGateCall = abi.encodeCall(IVaultV2.setSendAssetsGate, (vaultProvider));

        vaultContract.submit(receiveSharesGateCall);
        vaultContract.setReceiveSharesGate(vaultProvider);
        vaultContract.submit(sendSharesGateCall);
        vaultContract.setSendSharesGate(vaultProvider);
        vaultContract.submit(receiveAssetsGateCall);
        vaultContract.setReceiveAssetsGate(vaultProvider);
        vaultContract.submit(sendAssetsGateCall);
        vaultContract.setSendAssetsGate(vaultProvider);

        require(vaultContract.receiveSharesGate() == vaultProvider, "RECEIVE_SHARES_GATE_NOT_SET");
        require(vaultContract.sendSharesGate() == vaultProvider, "SEND_SHARES_GATE_NOT_SET");
        require(vaultContract.receiveAssetsGate() == vaultProvider, "RECEIVE_ASSETS_GATE_NOT_SET");
        require(vaultContract.sendAssetsGate() == vaultProvider, "SEND_ASSETS_GATE_NOT_SET");
        require(vaultContract.canReceiveShares(vaultProvider), "VAULT_PROVIDER_RECEIVE_SHARES_BLOCKED");
        require(vaultContract.canSendShares(vaultProvider), "VAULT_PROVIDER_SEND_SHARES_BLOCKED");
        require(vaultContract.canSendAssets(vaultProvider), "VAULT_PROVIDER_DEPOSIT_BLOCKED");
        require(vaultContract.canReceiveAssets(vaultProvider), "VAULT_PROVIDER_WITHDRAW_BLOCKED");

        vm.stopBroadcast();
    }
}
