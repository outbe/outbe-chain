// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

/// @dev CREATE3 salt version of the launch deploy, shared by every contract project. An environment
///      value overrides it only for a test run.
string constant SALT_VERSION = "v7.0.0";
