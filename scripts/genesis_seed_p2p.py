"""Strict encoding of validator network addresses."""

from __future__ import annotations
import ipaddress

from genesis_seed_codec import (
    MAX_P2P_ADDRESS_ENCODED_LEN,
    P2P_ADDRESS_VERSION_V1,
    parse_int,
)


def parse_host_port(value: str) -> tuple[str, int]:
    """Parse host:port, including [IPv6]:port."""
    if value.startswith("["):
        end = value.find("]")
        if end < 0 or end + 1 >= len(value) or value[end + 1] != ":":
            raise ValueError(f"invalid socket address: {value}")
        host = value[1:end]
        port_s = value[end + 2 :]
    else:
        if ":" not in value:
            raise ValueError(f"invalid socket address: {value}")
        host, port_s = value.rsplit(":", 1)
    try:
        port = int(port_s)
    except ValueError as exc:
        raise ValueError(f"invalid port in socket address: {value}") from exc
    if port <= 0 or port > 65535:
        raise ValueError(f"invalid port in socket address: {value}")
    return host, port


def validate_hostname(host: str):
    if not host or len(host) > 253:
        raise ValueError(f"invalid DNS hostname: {host}")
    for label in host.split("."):
        if not label or len(label) > 63:
            raise ValueError(f"invalid DNS hostname: {host}")
        if label[0] == "-" or label[-1] == "-":
            raise ValueError(f"invalid DNS hostname: {host}")
        if not all(ch.isascii() and (ch.isalnum() or ch == "-") for ch in label):
            raise ValueError(f"invalid DNS hostname: {host}")


def encode_p2p_socket(value: str) -> bytes:
    host, port = parse_host_port(value)
    ip = ipaddress.ip_address(host)
    if ip.version == 4:
        return bytes([4]) + ip.packed + port.to_bytes(2, "big")
    if ip.version == 6:
        return bytes([6]) + ip.packed + port.to_bytes(2, "big")
    raise ValueError(f"unsupported ip version in socket address: {value}")


def encode_p2p_ingress(value) -> bytes:
    if isinstance(value, str):
        return bytes([0]) + encode_p2p_socket(value)
    if isinstance(value, dict):
        if "socket" in value:
            return bytes([0]) + encode_p2p_socket(value["socket"])
        if "dns" in value:
            dns = value["dns"]
            host = dns["host"]
            port = parse_int(dns["port"])
            if port <= 0 or port > 65535:
                raise ValueError(f"invalid DNS ingress port: {port}")
            validate_hostname(host)
            encoded_host = host.encode()
            return (
                bytes([1])
                + len(encoded_host).to_bytes(2, "big")
                + encoded_host
                + port.to_bytes(2, "big")
            )
    raise ValueError(f"invalid p2p ingress value: {value}")


def encode_p2p_address_payload(value) -> tuple[int, bytes] | None:
    """Encode a validator p2p address seed into Outbe versioned bytes."""
    if value is None:
        return None
    if isinstance(value, str):
        payload = bytes([0]) + encode_p2p_socket(value)
    elif isinstance(value, dict):
        if "symmetric" in value:
            payload = bytes([0]) + encode_p2p_socket(value["symmetric"])
        elif "asymmetric" in value:
            asymmetric = value["asymmetric"]
            payload = (
                bytes([1])
                + encode_p2p_ingress(asymmetric["ingress"])
                + encode_p2p_socket(asymmetric["egress"])
            )
        else:
            raise ValueError(f"invalid p2p_address object: {value}")
    else:
        raise ValueError(f"invalid p2p_address value: {value}")
    if len(payload) > MAX_P2P_ADDRESS_ENCODED_LEN:
        raise ValueError(
            f"p2p_address payload exceeds {MAX_P2P_ADDRESS_ENCODED_LEN} bytes"
        )
    return P2P_ADDRESS_VERSION_V1, payload
