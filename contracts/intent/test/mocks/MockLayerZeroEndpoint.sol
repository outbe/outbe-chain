// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

struct Origin {
    uint32 srcEid;
    bytes32 sender;
    uint64 nonce;
}

struct MessagingParams {
    uint32 dstEid;
    bytes32 receiver;
    bytes message;
    bytes options;
    bool payInLzToken;
}

struct MessagingFee {
    uint256 nativeFee;
    uint256 lzTokenFee;
}

struct MessagingReceipt {
    bytes32 guid;
    uint64 nonce;
    MessagingFee fee;
}

interface ILzOAppV2 {
    function lzReceive(
        Origin calldata origin,
        bytes32 guid,
        bytes calldata message,
        address executor,
        bytes calldata extraData
    ) external payable;
}

/// @notice Упрощённый мок EndpointV2 для локальных тестов.
/// НЕ для продакшена.
contract EndpointV2Mock {
    uint32 public immutable eid;

    mapping(uint32 => EndpointV2Mock) public remoteEndpoints;
    mapping(uint32 => bytes32) public peers;

    address public oapp;
    address public delegate;

    error NotOApp();
    error RemoteEndpointNotSet(uint32 eid);
    error PeerNotSet(uint32 eid);

    modifier onlyOApp() {
        if (msg.sender != oapp) revert NotOApp();
        _;
    }

    constructor(uint32 _eid) {
        eid = _eid;
    }

    function setOApp(address _oapp) external {
        oapp = _oapp;
    }

    function setDelegate(address _delegate) external {
        delegate = _delegate;
    }

    function setRemoteEndpoint(uint32 _remoteEid, EndpointV2Mock _remoteEndpoint) external {
        remoteEndpoints[_remoteEid] = _remoteEndpoint;
    }

    function setPeer(uint32 _remoteEid, bytes32 _peer) external {
        peers[_remoteEid] = _peer;
    }

    function quote(
        MessagingParams calldata,
        /*_params*/
        address /*_sender*/
    )
        external
        pure
        returns (MessagingFee memory)
    {
        // Return fixed fee of 100 wei (very small for testing)
        return MessagingFee(100, 0);
    }

    function send(
        MessagingParams calldata _params,
        address /*_refundAddress*/
    )
        external
        payable
        onlyOApp
        returns (MessagingReceipt memory)
    {
        EndpointV2Mock dstEndpoint = remoteEndpoints[_params.dstEid];
        if (address(dstEndpoint) == address(0)) revert RemoteEndpointNotSet(_params.dstEid);

        bytes32 peer = peers[_params.dstEid];
        if (peer == bytes32(0)) {
            peer = _params.receiver;
            if (peer == bytes32(0)) revert PeerNotSet(_params.dstEid);
        }

        bytes32 sender = bytes32(uint256(uint160(oapp)));

        // Deliver message to destination endpoint
        dstEndpoint.deliverMessage(eid, sender, _params.message);

        // Return receipt
        return MessagingReceipt({
            guid: keccak256(abi.encodePacked(eid, sender, _params.dstEid)), nonce: 1, fee: MessagingFee(msg.value, 0)
        });
    }

    /// @notice External hook for delivering message from another endpoint
    function deliverMessage(uint32 _srcEid, bytes32 _sender, bytes calldata _message) external {
        Origin memory origin = Origin({srcEid: _srcEid, sender: _sender, nonce: 1});

        ILzOAppV2(oapp)
            .lzReceive(
                origin,
                keccak256(abi.encodePacked(_srcEid, _sender, eid)), // guid
                _message,
                msg.sender, // executor = calling endpoint
                bytes("")
            );
    }
}
