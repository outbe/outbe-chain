//! Upstream transport bounds and method fallback, tested against a real HTTP
//! JSON-RPC endpoint.
use super::{CallFailure, UpstreamLimits, UpstreamRpcClient, JSONRPC_METHOD_NOT_FOUND};
use commonware_consensus::types::Height;
use jsonrpsee::core::client::Error as ClientError;
use outbe_consensus::follow::{FinalizedSource as _, TipSource as _};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How the scripted upstream answers one method.
#[derive(Clone)]
enum Reply {
    /// Accept the request and never answer.
    Hang,
    /// Answer with a JSON-RPC error.
    Error(i32),
    /// Answer with this JSON `result`.
    Result(String),
}

type Methods = Arc<Mutex<Vec<String>>>;

/// A JSON-RPC-over-HTTP upstream that answers each method by script and
/// records every method it was asked for.
fn upstream(script: fn(&str) -> Reply) -> (String, Methods) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted upstream");
    let url = format!("http://{}", listener.local_addr().expect("local addr"));
    let methods = Methods::default();
    let seen = Arc::clone(&methods);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || serve(stream, script, &seen));
        }
    });
    (url, methods)
}

fn serve(stream: TcpStream, script: fn(&str) -> Reply, seen: &Methods) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    while let Some(request) = read_request(&mut reader) {
        let method = request["method"].as_str().unwrap_or_default().to_string();
        seen.lock().expect("methods lock").push(method.clone());
        let id = request["id"].clone();
        let response = match script(&method) {
            Reply::Hang => {
                std::thread::sleep(Duration::from_secs(5));
                break;
            }
            Reply::Error(code) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": code, "message": "scripted" },
            })
            .to_string(),
            Reply::Result(result) => format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#),
        };
        if write_response(&mut writer, &response).is_err() {
            break;
        }
    }
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<serde_json::Value> {
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(str::trim)
            .and_then(|value| value.parse().ok())
        {
            length = value;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn write_response(writer: &mut TcpStream, response: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        response.len()
    );
    writer.write_all(head.as_bytes())?;
    writer.write_all(response.as_bytes())
}

fn client(url: &str, request_timeout: Duration) -> UpstreamRpcClient {
    UpstreamRpcClient::with_limits(
        url,
        UpstreamLimits {
            request_timeout,
            ..UpstreamLimits::DEFAULT
        },
    )
    .expect("build upstream client")
}

fn recorded(methods: &Methods) -> Vec<String> {
    methods.lock().expect("methods lock").clone()
}

#[test]
fn production_limits_are_explicit() {
    assert_eq!(
        UpstreamLimits::DEFAULT.request_timeout,
        Duration::from_secs(10)
    );
    assert_eq!(UpstreamLimits::DEFAULT.max_response_bytes, 10 * 1024 * 1024);
}

#[test]
fn only_method_not_found_selects_the_legacy_method() {
    let call = |code| {
        ClientError::Call(jsonrpsee::types::ErrorObject::owned(
            code, "scripted", None::<()>,
        ))
    };
    assert_eq!(
        CallFailure::of(&call(JSONRPC_METHOD_NOT_FOUND)),
        CallFailure::MethodNotFound
    );
    for error in [
        call(-32000),
        call(-32603),
        ClientError::RequestTimeout,
        ClientError::Custom("transport".into()),
    ] {
        assert_eq!(CallFailure::of(&error), CallFailure::Transient);
    }
}

#[tokio::test]
async fn timeout_is_transient_and_never_falls_back() {
    let (url, methods) = upstream(|_| Reply::Hang);
    let upstream = client(&url, Duration::from_millis(200));
    for _ in 0..2 {
        let started = Instant::now();
        assert!(upstream.get_finality_proof(Height::new(5)).await.is_none());
        assert!(started.elapsed() < Duration::from_secs(4));
    }
    assert_eq!(
        recorded(&methods),
        ["outbe_getFinalityProof", "outbe_getFinalityProof"]
    );
}

#[tokio::test]
async fn method_not_found_falls_back_once_and_is_remembered() {
    let (url, methods) = upstream(|method| match method {
        "outbe_getFinalityProof" | "outbe_getConsensusBlock" => {
            Reply::Error(JSONRPC_METHOD_NOT_FOUND)
        }
        _ => Reply::Error(-32000),
    });
    let upstream = client(&url, Duration::from_secs(5));
    for _ in 0..2 {
        assert!(upstream.get_finality_proof(Height::new(5)).await.is_none());
        assert!(upstream.get_block(Height::new(5)).await.is_none());
    }
    assert_eq!(
        recorded(&methods),
        [
            "outbe_getFinalityProof",
            "outbe_getFinalization",
            "outbe_getConsensusBlock",
            "outbe_getFinalization",
            "outbe_getFinalization",
            "outbe_getFinalization",
        ]
    );
}

#[tokio::test]
async fn upstream_errors_do_not_switch_methods() {
    let (url, methods) = upstream(|_| Reply::Error(-32000));
    let upstream = client(&url, Duration::from_secs(5));
    assert!(upstream.get_finality_proof(Height::new(5)).await.is_none());
    assert!(upstream.get_block(Height::new(5)).await.is_none());
    assert_eq!(
        recorded(&methods),
        ["outbe_getFinalityProof", "outbe_getConsensusBlock"]
    );
}

#[tokio::test]
async fn responses_above_the_byte_cap_are_refused() {
    let (url, _methods) = upstream(|_| {
        Reply::Result(format!(
            r#"{{"lastFinalizedBlock":7,"padding":"{}"}}"#,
            "x".repeat(4096)
        ))
    });
    let capped = UpstreamRpcClient::with_limits(
        &url,
        UpstreamLimits {
            request_timeout: Duration::from_secs(5),
            max_response_bytes: 1024,
        },
    )
    .expect("build capped client");
    assert_eq!(capped.finalized_tip().await, None);
    let default = client(&url, Duration::from_secs(5));
    assert_eq!(default.finalized_tip().await, Some(Height::new(7)));
}
