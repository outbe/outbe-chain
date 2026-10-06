//! Transparent HTTP transport recording actual consensus-history requests.
use eyre::{ensure, eyre, Result};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

pub(super) struct ObservedUpstream {
    address: std::net::SocketAddr,
    path: PathBuf,
    stop: Arc<AtomicBool>,
    task: Option<JoinHandle<Result<()>>>,
}

impl ObservedUpstream {
    pub(super) fn start(upstream_port: u16, path: &Path) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let mut evidence = File::create(path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let task = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((client, _)) => forward(client, upstream_port, &mut evidence)?,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            evidence.sync_all()?;
            Ok(())
        });
        Ok(Self {
            address,
            path: path.into(),
            stop,
            task: Some(task),
        })
    }

    pub(super) fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub(super) fn finish(mut self, anchor: u64) -> Result<Vec<u64>> {
        self.join()?;
        let mut heights = Vec::new();
        for line in std::fs::read_to_string(&self.path)?.lines() {
            let request: serde_json::Value = serde_json::from_str(line)?;
            if let Some(height) = request["height"].as_u64() {
                heights.push(height);
            }
        }
        ensure!(
            anchor > 1 && !heights.is_empty(),
            "no observed non-genesis consensus recovery"
        );
        ensure!(
            heights.iter().all(|height| *height >= anchor),
            "FullNode fetched consensus history before anchor {anchor}: {heights:?}"
        );
        Ok(heights)
    }

    fn join(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            task.join()
                .map_err(|_| eyre!("upstream observer panicked"))??;
        }
        Ok(())
    }
}

impl Drop for ObservedUpstream {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

fn forward(mut client: TcpStream, port: u16, evidence: &mut File) -> Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(30)))?;
    client.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut input = BufReader::new(&mut client);
    let mut header_bytes = 0;
    let mut content_length = None;
    loop {
        let mut line = String::new();
        ensure!(input.read_line(&mut line)? > 0, "incomplete HTTP request");
        header_bytes += line.len();
        ensure!(header_bytes <= 16 * 1024, "HTTP headers exceed bound");
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse::<usize>()?);
            }
        }
    }
    let length = content_length.ok_or_else(|| eyre!("missing HTTP Content-Length"))?;
    ensure!(length <= MAX_REQUEST_BYTES, "HTTP body exceeds bound");
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    let request: serde_json::Value = serde_json::from_slice(&body)?;
    let method = request["method"]
        .as_str()
        .ok_or_else(|| eyre!("missing RPC method"))?;
    let height = matches!(
        method,
        "outbe_getFinalityProof" | "outbe_getFinalization" | "outbe_getConsensusBlock"
    )
    .then(|| request["params"][0].as_u64())
    .flatten();
    if matches!(
        method,
        "outbe_getFinalityProof" | "outbe_getFinalization" | "outbe_getConsensusBlock"
    ) {
        ensure!(height.is_some(), "unrecognized consensus request height");
    }
    writeln!(
        evidence,
        "{}",
        serde_json::json!({"method":method,"height":height})
    )?;
    evidence.flush()?;
    let mut upstream = TcpStream::connect(("127.0.0.1", port))?;
    upstream.set_read_timeout(Some(Duration::from_secs(30)))?;
    upstream.set_write_timeout(Some(Duration::from_secs(30)))?;
    write!(upstream, "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n")?;
    upstream.write_all(&body)?;
    let copied = std::io::copy(&mut upstream.take(MAX_RESPONSE_BYTES + 1), &mut client)?;
    ensure!(copied <= MAX_RESPONSE_BYTES, "HTTP response exceeds bound");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(height: u64) -> Result<Vec<u64>> {
        let directory = tempfile::tempdir()?;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let body = serde_json::json!({"jsonrpc":"2.0","id":7,
            "method":"outbe_getFinalization","params":[height]})
        .to_string();
        let expected = body.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut input = BufReader::new(&mut stream);
            let mut line = String::new();
            while input.read_line(&mut line).unwrap() > 0 {
                if line == "\r\n" {
                    break;
                }
                line.clear();
            }
            let mut received = vec![0; expected.len()];
            input.read_exact(&mut received).unwrap();
            assert_eq!(received, expected.as_bytes());
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnull")
                .unwrap();
        });
        let observer = ObservedUpstream::start(port, &directory.path().join("requests.jsonl"))?;
        let mut client = TcpStream::connect(observer.address)?;
        client.set_read_timeout(Some(Duration::from_secs(5)))?;
        write!(
            client,
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )?;
        let mut response = String::new();
        client.read_to_string(&mut response)?;
        assert_eq!(
            response,
            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnull"
        );
        server.join().unwrap();
        observer.finish(5)
    }

    #[test]
    fn forwards_rpc_bytes_and_records_consensus_height() {
        assert_eq!(round_trip(5).unwrap(), vec![5]);
    }

    #[test]
    fn rejects_a_real_request_before_the_anchor() {
        assert!(round_trip(4)
            .unwrap_err()
            .to_string()
            .contains("before anchor 5"));
    }
}
