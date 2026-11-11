//! Key request to the dstack guest agent of the VM that runs this service.
//!
//! The guest agent listens on a Unix socket and answers HTTP requests. The
//! `GetKey` call derives a key from the application keys of the VM. Which
//! party can learn the application keys depends on the key provider that the
//! application manifest selects:
//!
//! | Key provider | Application keys |
//! |---|---|
//! | `none` | random at every boot: the derived key changes after a restart |
//! | `local` | released by the key provider enclave of the host for the measurements of the VM |
//! | `kms` | released by a dstack key management service |
//!
//! This module makes one blocking request at startup. It does not check the
//! signature chain of the response: the agent runs inside the same VM.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use zeroize::Zeroizing;

use crate::errors::{ConfidentialVmError, EnclaveResult};

/// Environment variable that overrides the socket path of the guest agent.
pub const GUEST_AGENT_SOCKET_ENV: &str = "OUTBE_DSTACK_AGENT_SOCKET";
const DEFAULT_SOCKET: &str = "/var/run/dstack.sock";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on the HTTP response. A key response is below one kilobyte.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
const KEY_BYTES: usize = 32;
const KEY_PURPOSE: &str = "outbe-tee-seal";

#[derive(Deserialize)]
struct GetKeyResponse {
    /// The derived key as hexadecimal text.
    key: String,
}

/// Request the key that the guest agent derives for `path`.
pub(crate) fn get_key(path: &str) -> EnclaveResult<Zeroizing<Vec<u8>>> {
    let socket = std::env::var_os(GUEST_AGENT_SOCKET_ENV)
        .map_or_else(|| PathBuf::from(DEFAULT_SOCKET), PathBuf::from);
    get_key_from(&socket, path)
}

fn get_key_from(socket: &Path, path: &str) -> EnclaveResult<Zeroizing<Vec<u8>>> {
    let response = request(socket, path)
        .map_err(|error| ConfidentialVmError::guest_agent_key(format_args!("{error}")))?;
    parse_response(&response).map_err(|reason| ConfidentialVmError::guest_agent_key(reason).into())
}

fn request(socket: &Path, path: &str) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let body = serde_json::json!({ "path": path, "purpose": KEY_PURPOSE }).to_string();
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    write!(
        stream,
        "POST /GetKey HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()?;
    // The response carries a secret: keep it in a buffer that is cleared.
    let mut response = Zeroizing::new(Vec::new());
    stream.take(MAX_RESPONSE_BYTES).read_to_end(&mut response)?;
    Ok(response)
}

/// Read the key from a complete HTTP/1.1 response.
fn parse_response(response: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("response has no header end")?;
    let head = std::str::from_utf8(&response[..header_end])
        .map_err(|_| "response header is not text".to_owned())?;
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    if status.split(' ').nth(1) != Some("200") {
        return Err(format!("status line: {status}"));
    }
    let chunked = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("transfer-encoding")
                && value.trim().eq_ignore_ascii_case("chunked")
        })
    });
    let raw_body = &response[header_end + 4..];
    let body = if chunked {
        Zeroizing::new(dechunk(raw_body)?)
    } else {
        Zeroizing::new(raw_body.to_vec())
    };
    let parsed: GetKeyResponse =
        serde_json::from_slice(&body).map_err(|_| "response body is not a key response")?;
    let key_text = Zeroizing::new(parsed.key);
    let key = Zeroizing::new(
        hex::decode(key_text.trim_start_matches("0x"))
            .map_err(|_| "key is not hexadecimal text".to_owned())?,
    );
    if key.len() != KEY_BYTES || key.iter().all(|byte| *byte == 0) {
        return Err(format!("key has {} bytes or is zero", key.len()));
    }
    Ok(key)
}

/// Join the chunks of a body with chunked transfer encoding.
fn dechunk(mut input: &[u8]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let line_end = input
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or("chunk size line is not complete")?;
        let size_text = std::str::from_utf8(&input[..line_end])
            .map_err(|_| "chunk size is not text".to_owned())?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size =
            usize::from_str_radix(size_text, 16).map_err(|_| "chunk size is not hexadecimal")?;
        input = &input[line_end + 2..];
        if size == 0 {
            return Ok(body);
        }
        let chunk = input.get(..size).ok_or("chunk is not complete")?;
        body.extend_from_slice(chunk);
        input = input
            .get(size..)
            .and_then(|rest| rest.strip_prefix(b"\r\n"))
            .ok_or("chunk has no end")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn body() -> String {
        format!(r#"{{"key":"{KEY_HEX}","signature_chain":["aa","bb"]}}"#)
    }

    fn plain_response(status: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn reads_the_key_of_a_plain_response() {
        let key = parse_response(&plain_response("200 OK", &body())).unwrap();
        assert_eq!(*key, hex::decode(KEY_HEX).unwrap());
    }

    #[test]
    fn reads_the_key_of_a_chunked_response() {
        let body = body();
        let (first, second) = body.split_at(20);
        let response = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{first}\r\n{:x}\r\n{second}\r\n0\r\n\r\n",
            first.len(),
            second.len()
        );
        let key = parse_response(response.as_bytes()).unwrap();
        assert_eq!(*key, hex::decode(KEY_HEX).unwrap());
    }

    #[test]
    fn rejects_error_status_and_malformed_bodies() {
        assert!(parse_response(&plain_response("500 Internal Server Error", &body())).is_err());
        assert!(parse_response(b"HTTP/1.1 200 OK\r\n").is_err());
        assert!(parse_response(&plain_response("200 OK", "not json")).is_err());
        assert!(parse_response(&plain_response("200 OK", r#"{"key":"zz"}"#)).is_err());
        assert!(parse_response(&plain_response("200 OK", r#"{"key":"0011"}"#)).is_err());
        let zero = format!(r#"{{"key":"{}"}}"#, "00".repeat(32));
        assert!(parse_response(&plain_response("200 OK", &zero)).is_err());
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab")
                .is_err()
        );
    }

    #[test]
    fn requests_the_key_over_a_unix_socket() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("agent.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            // Read the header, then the body that Content-Length declares.
            loop {
                let read = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request).into_owned();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = head
                        .lines()
                        .find_map(|line| line.strip_prefix("Content-Length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if body.len() >= length {
                        break;
                    }
                }
            }
            stream
                .write_all(&plain_response("200 OK", &body()))
                .unwrap();
            String::from_utf8(request).unwrap()
        });

        let key = get_key_from(&socket, "outbe/test").unwrap();
        assert_eq!(*key, hex::decode(KEY_HEX).unwrap());
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /GetKey HTTP/1.1\r\n"));
        assert!(request.ends_with(r#"{"path":"outbe/test","purpose":"outbe-tee-seal"}"#));
    }

    #[test]
    fn absent_socket_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        assert!(get_key_from(&directory.path().join("absent.sock"), "outbe/test").is_err());
    }
}
