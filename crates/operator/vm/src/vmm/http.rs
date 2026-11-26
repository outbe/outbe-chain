//! Minimal HTTP/1.1 request framing and bounded response parsing for the dstack VMM socket.

use std::io::{BufRead, Read};

use eyre::{bail, ensure, Context, Result};

pub(super) const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
pub(super) const MAX_HTTP_BODY_BYTES: usize = 32 * 1024 * 1024;
pub(super) const HTTP_READ_BUF_CAPACITY: usize = 8 * 1024;

/// One HTTP/1.1 request sent over a fresh `Connection: close` stream.
pub(super) struct HttpRequest<'a> {
    pub(super) method: &'a str,
    pub(super) target: &'a str,
    pub(super) content_type: Option<&'a str>,
    pub(super) body: &'a [u8],
}

impl HttpRequest<'_> {
    /// Render the request line and headers, including the terminating blank line.
    pub(super) fn head(&self) -> String {
        let mut head = format!(
            "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n",
            self.method, self.target
        );
        if let Some(ct) = self.content_type {
            head.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        if self.method == "POST" || !self.body.is_empty() {
            head.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        }
        head.push_str("\r\n");
        head
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ParsedHttpResponse {
    pub(super) status_code: u16,
    pub(super) body: Vec<u8>,
}

/// Body framing that the response headers declare.
#[derive(Default)]
struct BodyFraming {
    content_length: Option<usize>,
    is_chunked: bool,
}

impl BodyFraming {
    /// Record the framing information of one `name: value` header line.
    fn apply_header(&mut self, line: &str) -> Result<()> {
        let Some((raw_name, raw_value)) = line.split_once(':') else {
            bail!("malformed HTTP header line: {line}");
        };
        let name = raw_name.trim().to_ascii_lowercase();
        let value = raw_value.trim();
        match name.as_str() {
            "content-length" => self.set_content_length(value)?,
            "transfer-encoding" if has_chunked_token(value) => self.is_chunked = true,
            _ => {}
        }
        Ok(())
    }

    fn set_content_length(&mut self, value: &str) -> Result<()> {
        let parsed: usize = value
            .parse()
            .wrap_err_with(|| format!("invalid Content-Length header: {value}"))?;
        if let Some(prev) = self.content_length {
            ensure!(
                prev == parsed,
                "conflicting Content-Length headers ({prev} vs {parsed})"
            );
        }
        self.content_length = Some(parsed);
        Ok(())
    }

    fn read_body<R: BufRead>(&self, reader: &mut R) -> Result<Vec<u8>> {
        if self.is_chunked {
            return read_chunked_body(reader);
        }
        match self.content_length {
            Some(len) => read_sized_body(reader, len),
            None => read_eof_body(reader),
        }
    }
}

fn has_chunked_token(transfer_encoding: &str) -> bool {
    transfer_encoding
        .split(',')
        .any(|token| token.trim().eq_ignore_ascii_case("chunked"))
}

/// Parse an HTTP/1.x response supporting `Content-Length`, `Transfer-Encoding: chunked`,
/// and EOF-terminated `Connection: close` bodies.
pub(super) fn parse_http_response<R: BufRead>(reader: &mut R) -> Result<ParsedHttpResponse> {
    let mut header_bytes = 0usize;
    let status_line = read_bounded_line(reader, &mut header_bytes, MAX_HTTP_HEADER_BYTES)?;
    let status_code = parse_http_status_line(&status_line)?;

    let mut framing = BodyFraming::default();
    loop {
        let line = read_bounded_line(reader, &mut header_bytes, MAX_HTTP_HEADER_BYTES)?;
        if line.is_empty() {
            break;
        }
        framing.apply_header(&line)?;
    }

    let body = framing.read_body(reader)?;
    Ok(ParsedHttpResponse { status_code, body })
}

fn read_sized_body<R: BufRead>(reader: &mut R, len: usize) -> Result<Vec<u8>> {
    ensure!(
        len <= MAX_HTTP_BODY_BYTES,
        "HTTP response Content-Length {len} exceeds maximum {MAX_HTTP_BODY_BYTES}"
    );
    // Read incrementally rather than pre-allocating `len` bytes up front.
    let mut buf = Vec::with_capacity(len.min(MAX_HTTP_HEADER_BYTES));
    let mut limited = reader.take(len as u64);
    limited
        .read_to_end(&mut buf)
        .wrap_err("read HTTP response body")?;
    ensure!(
        buf.len() == len,
        "truncated HTTP response body (expected {len} bytes, got {})",
        buf.len()
    );
    Ok(buf)
}

fn read_eof_body<R: BufRead>(reader: &mut R) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut limited = reader.take((MAX_HTTP_BODY_BYTES + 1) as u64);
    limited
        .read_to_end(&mut buf)
        .wrap_err("read EOF-terminated HTTP response body")?;
    ensure!(
        buf.len() <= MAX_HTTP_BODY_BYTES,
        "HTTP response body exceeds maximum {MAX_HTTP_BODY_BYTES} bytes"
    );
    Ok(buf)
}

/// Read one LF-terminated HTTP header/framing line without ever allocating past `max_total_bytes`.
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    total_bytes: &mut usize,
    max_total_bytes: usize,
) -> Result<String> {
    let mut line = Vec::new();
    loop {
        let remaining = max_total_bytes.saturating_sub(*total_bytes);
        ensure!(
            remaining > 0,
            "HTTP response headers exceed maximum {max_total_bytes} bytes"
        );
        let available = reader.fill_buf().wrap_err("read HTTP header line")?;
        if available.is_empty() {
            bail!("unexpected EOF while reading HTTP response headers");
        }
        let scan_len = available.len().min(remaining);
        if let Some(pos) = available[..scan_len].iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            line.extend_from_slice(&available[..take]);
            reader.consume(take);
            *total_bytes += take;
            break;
        }
        if available.len() >= remaining {
            bail!("HTTP response headers exceed maximum {max_total_bytes} bytes");
        }
        let take = available.len();
        line.extend_from_slice(available);
        reader.consume(take);
        *total_bytes += take;
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    String::from_utf8(line).wrap_err("non-UTF-8 bytes in HTTP response header")
}

fn parse_http_status_line(line: &str) -> Result<u16> {
    let mut parts = line.split_whitespace();
    let version = parts
        .next()
        .ok_or_else(|| eyre::eyre!("empty HTTP status line"))?;
    ensure!(
        version.starts_with("HTTP/1."),
        "unsupported HTTP version in status line: {line}"
    );
    let code_str = parts
        .next()
        .ok_or_else(|| eyre::eyre!("missing HTTP status code in status line: {line}"))?;
    ensure!(
        code_str.len() == 3 && code_str.bytes().all(|b| b.is_ascii_digit()),
        "invalid HTTP status code in status line: {line}"
    );
    code_str
        .parse::<u16>()
        .wrap_err_with(|| format!("parse HTTP status code from: {line}"))
}

fn read_chunked_body<R: BufRead>(reader: &mut R) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut framing_bytes = 0usize;
    loop {
        let size_line = read_bounded_line(reader, &mut framing_bytes, MAX_HTTP_HEADER_BYTES)?;
        let chunk_size = parse_chunk_size(&size_line)?;
        if chunk_size == 0 {
            skip_chunk_trailers(reader, &mut framing_bytes)?;
            return Ok(out);
        }
        read_chunk_payload(reader, &mut out, chunk_size)?;
    }
}

fn parse_chunk_size(size_line: &str) -> Result<usize> {
    let size_hex = size_line.split(';').next().unwrap_or("").trim();
    ensure!(!size_hex.is_empty(), "empty HTTP chunk size line");
    usize::from_str_radix(size_hex, 16)
        .wrap_err_with(|| format!("invalid HTTP chunk size: {size_hex}"))
}

/// Consume optional trailer headers until the terminating blank line.
fn skip_chunk_trailers<R: BufRead>(reader: &mut R, framing_bytes: &mut usize) -> Result<()> {
    loop {
        let trailer = read_bounded_line(reader, framing_bytes, MAX_HTTP_HEADER_BYTES)?;
        if trailer.is_empty() {
            return Ok(());
        }
    }
}

/// Append one chunk payload to `out` and consume the CRLF that follows it.
fn read_chunk_payload<R: BufRead>(
    reader: &mut R,
    out: &mut Vec<u8>,
    chunk_size: usize,
) -> Result<()> {
    let next_len = out
        .len()
        .checked_add(chunk_size)
        .ok_or_else(|| eyre::eyre!("HTTP chunked body size overflow"))?;
    ensure!(
        next_len <= MAX_HTTP_BODY_BYTES,
        "HTTP chunked body exceeds maximum {MAX_HTTP_BODY_BYTES} bytes"
    );
    let start = out.len();
    out.resize(next_len, 0);
    reader
        .read_exact(&mut out[start..next_len])
        .wrap_err("truncated HTTP chunk payload")?;
    let mut crlf = [0u8; 2];
    reader
        .read_exact(&mut crlf)
        .wrap_err("missing CRLF after HTTP chunk payload")?;
    ensure!(
        crlf == *b"\r\n",
        "expected CRLF after HTTP chunk payload, got {crlf:?}"
    );
    Ok(())
}

/// Build the VMM `/logs` request target (path and query) for one VM serial log tail.
pub(super) fn serial_logs_target(vm_id: &str, lines: usize) -> String {
    let encoded_id = percent_encode_query_component(vm_id);
    format!("/logs?id={encoded_id}&follow=false&ansi=false&lines={lines}")
}

fn percent_encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}
