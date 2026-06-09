// SPDX-License-Identifier: GPL-2.0-or-later
// Copyright (c) 2025 Morpho Association
pragma solidity >=0.5.0;

// TODO replace this copy-paste with reference from Morpho libs
interface IReceiveSharesGate {
    function canReceiveShares(address account) external view returns (bool);
}

interface ISendSharesGate {
    function canSendShares(address account) external view returns (bool);
}

interface IReceiveAssetsGate {
    function canReceiveAssets(address account) external view returns (bool);
}

interface ISendAssetsGate {
    function canSendAssets(address account) external view returns (bool);
}
