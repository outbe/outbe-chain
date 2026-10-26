//! Strict encoding of validator network addresses.

use std::net::IpAddr;

use serde_json::Value;

use crate::error::GenesisError;
use crate::seed_codec::{parse_u64, MAX_P2P_ADDRESS_ENCODED_LEN, P2P_ADDRESS_VERSION_V1};

/// Parse `host:port`, including `[IPv6]:port`.
pub fn parse_host_port(value: &str) -> Result<(String, u16), GenesisError> {
    let (host, port_s) = if let Some(rest) = value.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return Err(GenesisError::validation(format!(
                "invalid socket address: {value}"
            )));
        };
        let after = &rest[end + 1..];
        let Some(port_part) = after.strip_prefix(':') else {
            return Err(GenesisError::validation(format!(
                "invalid socket address: {value}"
            )));
        };
        (rest[..end].to_owned(), port_part)
    } else {
        let Some((h, p)) = value.rsplit_once(':') else {
            return Err(GenesisError::validation(format!(
                "invalid socket address: {value}"
            )));
        };
        (h.to_owned(), p)
    };
    let port: u32 = port_s.parse().map_err(|_| {
        GenesisError::validation(format!("invalid port in socket address: {value}"))
    })?;
    if !(1..=65535).contains(&port) {
        return Err(GenesisError::validation(format!(
            "invalid port in socket address: {value}"
        )));
    }
    Ok((host, port as u16))
}

/// Validate a DNS hostname according to RFC 1123 label rules.
pub fn validate_hostname(host: &str) -> Result<(), GenesisError> {
    if host.is_empty() || host.len() > 253 {
        return Err(GenesisError::validation(format!(
            "invalid DNS hostname: {host}"
        )));
    }
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(GenesisError::validation(format!(
                "invalid DNS hostname: {host}"
            )));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(GenesisError::validation(format!(
                "invalid DNS hostname: {host}"
            )));
        }
        if !label
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        {
            return Err(GenesisError::validation(format!(
                "invalid DNS hostname: {host}"
            )));
        }
    }
    Ok(())
}

/// Encode a `host:port` IP socket address into `[ip_version, ip_bytes.., port_be16]`.
pub fn encode_p2p_socket(value: &str) -> Result<Vec<u8>, GenesisError> {
    let (host, port) = parse_host_port(value)?;
    let ip: IpAddr = host
        .parse()
        .map_err(|_| GenesisError::validation(format!("invalid IP in socket address: {value}")))?;
    let mut out = Vec::with_capacity(19);
    match ip {
        IpAddr::V4(v4) => {
            out.push(4);
            out.extend_from_slice(&v4.octets());
        }
        IpAddr::V6(v6) => {
            out.push(6);
            out.extend_from_slice(&v6.octets());
        }
    }
    out.extend_from_slice(&port.to_be_bytes());
    Ok(out)
}

/// Encode a P2P ingress descriptor (`"ip:port"`, `{"socket": ...}`, or `{"dns": {"host": ..., "port": ...}}`).
pub fn encode_p2p_ingress(value: &Value) -> Result<Vec<u8>, GenesisError> {
    if let Some(s) = value.as_str() {
        let mut out = vec![0u8];
        out.extend_from_slice(&encode_p2p_socket(s)?);
        return Ok(out);
    }
    if let Some(obj) = value.as_object() {
        if let Some(socket) = obj.get("socket").and_then(Value::as_str) {
            let mut out = vec![0u8];
            out.extend_from_slice(&encode_p2p_socket(socket)?);
            return Ok(out);
        }
        if let Some(dns) = obj.get("dns").and_then(Value::as_object) {
            let host = dns
                .get("host")
                .and_then(Value::as_str)
                .ok_or_else(|| GenesisError::validation("missing dns.host"))?;
            let port_val = dns
                .get("port")
                .ok_or_else(|| GenesisError::validation("missing dns.port"))?;
            let port = parse_u64(port_val)?;
            if !(1..=65535).contains(&port) {
                return Err(GenesisError::validation(format!(
                    "invalid DNS ingress port: {port}"
                )));
            }
            validate_hostname(host)?;
            let encoded_host = host.as_bytes();
            let mut out = Vec::with_capacity(1 + 2 + encoded_host.len() + 2);
            out.push(1);
            out.extend_from_slice(&(encoded_host.len() as u16).to_be_bytes());
            out.extend_from_slice(encoded_host);
            out.extend_from_slice(&(port as u16).to_be_bytes());
            return Ok(out);
        }
    }
    Err(GenesisError::validation(format!(
        "invalid p2p ingress value: {value}"
    )))
}

/// Encode a validator `p2p_address` seed into Outbe versioned bytes `(version, payload)`.
pub fn encode_p2p_address_payload(
    value: Option<&Value>,
) -> Result<Option<(u64, Vec<u8>)>, GenesisError> {
    let Some(val) = value else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(None);
    }
    let payload = if let Some(s) = val.as_str() {
        let mut buf = vec![0u8];
        buf.extend_from_slice(&encode_p2p_socket(s)?);
        buf
    } else if let Some(obj) = val.as_object() {
        if let Some(sym) = obj.get("symmetric").and_then(Value::as_str) {
            let mut buf = vec![0u8];
            buf.extend_from_slice(&encode_p2p_socket(sym)?);
            buf
        } else if let Some(asym) = obj.get("asymmetric").and_then(Value::as_object) {
            let ingress = asym
                .get("ingress")
                .ok_or_else(|| GenesisError::validation("missing asymmetric.ingress"))?;
            let egress = asym
                .get("egress")
                .and_then(Value::as_str)
                .ok_or_else(|| GenesisError::validation("missing asymmetric.egress"))?;
            let mut buf = vec![1u8];
            buf.extend_from_slice(&encode_p2p_ingress(ingress)?);
            buf.extend_from_slice(&encode_p2p_socket(egress)?);
            buf
        } else {
            return Err(GenesisError::validation(format!(
                "invalid p2p_address object: {val}"
            )));
        }
    } else {
        return Err(GenesisError::validation(format!(
            "invalid p2p_address value: {val}"
        )));
    };
    if payload.len() > MAX_P2P_ADDRESS_ENCODED_LEN {
        return Err(GenesisError::validation(format!(
            "p2p_address payload exceeds {MAX_P2P_ADDRESS_ENCODED_LEN} bytes"
        )));
    }
    Ok(Some((P2P_ADDRESS_VERSION_V1, payload)))
}
