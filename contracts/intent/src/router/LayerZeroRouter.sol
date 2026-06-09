// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import { OApp, Origin, MessagingFee } from "@layerzerolabs/lz-evm-oapp-v2/contracts/oapp/OApp.sol";
import { OptionsBuilder } from "@layerzerolabs/lz-evm-oapp-v2/contracts/oapp/libs/OptionsBuilder.sol";
import { Ownable } from "@openzeppelin/contracts/access/Ownable.sol";
import { Ownable2Step } from "@openzeppelin/contracts/access/Ownable2Step.sol";
import { BaseRouter } from "./BaseRouter.sol";
import { RouterMessage } from "../libs/RouterMessage.sol";

/**
 * @title LayerZeroRouter
 * @notice Auction-based ERC-7683 router using LayerZero V2 for cross-chain messaging.
 * @dev Inherits all settlement logic from BaseRouter.
 *      Only LayerZero-specific concerns live here:
 *        - eid ↔ domain mapping
 *        - _dispatchSettleCrossChain / _dispatchRefundCrossChain via _lzSend
 *        - _lzReceive handler
 *        - _localDomain via endpoint.eid()
 */
contract LayerZeroRouter is BaseRouter, OApp {
    using OptionsBuilder for bytes;

    // ============ Storage ============

    /// @notice Mapping from domain ID to LayerZero endpoint ID (eid)
    mapping(uint32 => uint32) public domainToEid;

    /// @notice Mapping from LayerZero endpoint ID to domain ID
    mapping(uint32 => uint32) public eidToDomain;

    /// @notice Base gas limit for destination execution (per-message overhead)
    uint128 public defaultGasLimit;

    /// @notice Additional destination gas granted per order in a settle/refund batch
    uint128 public perOrderGas;

    // ============ Events ============

    event MessageReceived(uint32 indexed srcEid, bytes32 indexed sender, bytes payload);
    event GasLimitUpdated(uint128 oldLimit, uint128 newLimit);
    event PerOrderGasUpdated(uint128 oldGas, uint128 newGas);

    // ============ Errors ============

    /// @notice Thrown when the LayerZero endpoint address is zero
    error InvalidEndpoint();

    // ============ Constructor ============

    /**
     * @notice Initializes the LayerZeroRouter contract.
     * @param _lzEndpoint The LayerZero V2 endpoint address.
     * @param _owner      The contract owner (admin).
     * @param _compact    The Compact contract address.
     * @param _lockTag    Resource lock tag from RouterAllocator.buildLockTag().
     * @param _escrow     SolverEscrow address (address(0) to disable collateral checks).
     * @param _auction    Auction contract address (fixed for the router's lifetime).
     */
    constructor(
        address _lzEndpoint,
        address _owner,
        address _compact,
        bytes12 _lockTag,
        address _escrow,
        address _auction
    )
        OApp(_lzEndpoint, _owner)
        BaseRouter(_compact, _lockTag, _escrow, _auction)
    {
        if (_lzEndpoint == address(0)) revert InvalidEndpoint();
        defaultGasLimit = 200_000;
        perOrderGas = 60_000;

        // Transfer ownership if deploying via CreateX
        if (msg.sender != _owner) {
            _transferOwnership(_owner);
        }
    }

    // ========== OWNERSHIP (Ownable2Step over OApp's Ownable) ==========

    /// @dev Resolves the diamond inheritance between BaseRouter's Ownable2Step and OApp's Ownable.
    function transferOwnership(address newOwner) public override(Ownable, Ownable2Step) onlyOwner {
        Ownable2Step.transferOwnership(newOwner);
    }

    function _transferOwnership(address newOwner) internal override(Ownable, Ownable2Step) {
        Ownable2Step._transferOwnership(newOwner);
    }

    // ========== CONFIGURATION ==========

    /**
     * @notice Registers a remote chain peer and its domain mapping.
     * @param _eid    LayerZero endpoint ID of the remote chain.
     * @param _peer   Address of the peer router on the remote chain (bytes32).
     * @param _domain Domain ID corresponding to this endpoint.
     */
    function setPeerWithDomain(uint32 _eid, bytes32 _peer, uint32 _domain) public onlyOwner {
        _setPeer(_eid, _peer);
        domainToEid[_domain] = _eid;
        eidToDomain[_eid] = _domain;
    }

    /**
     * @notice Updates the default gas limit for cross-chain messages.
     * @param _newLimit The new gas limit.
     */
    function setDefaultGasLimit(uint128 _newLimit) external onlyOwner {
        uint128 oldLimit = defaultGasLimit;
        defaultGasLimit = _newLimit;
        emit GasLimitUpdated(oldLimit, _newLimit);
    }

    /**
     * @notice Updates the per-order gas added to the destination execution option for a batch.
     * @param _newPerOrderGas The new per-order gas.
     */
    function setPerOrderGas(uint128 _newPerOrderGas) external onlyOwner {
        uint128 oldGas = perOrderGas;
        perOrderGas = _newPerOrderGas;
        emit PerOrderGasUpdated(oldGas, _newPerOrderGas);
    }

    // ========== MESSAGING — OUTBOUND ==========

    /**
     * @notice Quotes the LayerZero fee for sending a payload to a destination domain.
     * @param _dstDomain    Destination domain ID.
     * @param _payload      Message payload.
     * @param _payInLzToken Whether to pay in LZ token.
     * @return fee          The messaging fee breakdown.
     */
    function quote(
        uint32 _dstDomain,
        bytes memory _payload,
        bool _payInLzToken
    )
        external
        view
        returns (MessagingFee memory fee)
    {
        uint32 dstEid = domainToEid[_dstDomain];
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(defaultGasLimit, 0);
        return _quote(dstEid, _payload, options, _payInLzToken);
    }

    function _dispatchSettleCrossChain(
        uint32 _originDomain,
        bytes32[] memory _orderIds,
        bytes[] memory _ordersFillerData
    )
        internal
        override
    {
        uint32 dstEid = domainToEid[_originDomain];
        bytes memory payload = RouterMessage.encodeSettle(_orderIds, _ordersFillerData);
        uint128 gasLimit = defaultGasLimit + perOrderGas * uint128(_orderIds.length);
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(gasLimit, 0);
        _lzSend(dstEid, payload, options, MessagingFee(msg.value, 0), payable(msg.sender));
    }

    function _dispatchRefundCrossChain(uint32 _originDomain, bytes32[] memory _orderIds) internal override {
        uint32 dstEid = domainToEid[_originDomain];
        bytes memory payload = RouterMessage.encodeRefund(_orderIds);
        uint128 gasLimit = defaultGasLimit + perOrderGas * uint128(_orderIds.length);
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(gasLimit, 0);
        _lzSend(dstEid, payload, options, MessagingFee(msg.value, 0), payable(msg.sender));
    }

    // ========== MESSAGING — INBOUND ==========

    function _lzReceive(
        Origin calldata _origin,
        bytes32, /* _guid */
        bytes calldata _payload,
        address, /* _executor */
        bytes calldata /* _extraData */
    )
        internal
        override
    {
        emit MessageReceived(_origin.srcEid, _origin.sender, _payload);

        (bool _settle, bytes32[] memory _orderIds, bytes[] memory _ordersFillerData) = RouterMessage.decode(_payload);

        uint32 originDomain = eidToDomain[_origin.srcEid];
        bytes32 messageSender = _origin.sender;

        for (uint256 i = 0; i < _orderIds.length; i++) {
            if (_settle) {
                _handleSettleOrder(
                    originDomain, messageSender, _orderIds[i], abi.decode(_ordersFillerData[i], (bytes32))
                );
            } else {
                _handleRefundOrder(originDomain, messageSender, _orderIds[i]);
            }
        }
    }

    // ========== LOCAL DOMAIN ==========

    function _localDomain() internal view virtual override returns (uint32) {
        return eidToDomain[endpoint.eid()];
    }
}
