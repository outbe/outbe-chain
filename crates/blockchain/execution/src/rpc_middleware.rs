//! Node-internal JSON-RPC batch size cap and fail-fast heavy-method admission middleware.
//!
//! Reth configures `jsonrpsee` with `BatchRequestConfig::Unlimited` by default, allowing a
//! single 15 MiB HTTP or WebSocket payload to pack thousands of JSON-RPC calls. This layer:
//! 1. Caps batch length at [`OutbeRpcProtectionConfig::max_batch_requests`] (`-32010`).
//! 2. Routes batch entries sequentially through [`OutbeRpcMiddleware::call`] so batched calls
//!    cannot bypass per-class heavy-method admission gates.
//! 3. Enforces fail-fast concurrency semaphores (`-32005`, EIP-1474) on heavy EVM/proof/trace
//!    methods while leaving lightweight status/submission calls and consensus follower sync
//!    (`outbe_getFinalityProof`, which has its own bounded bridge queue) unobstructed.

use std::future::Future;
use std::sync::Arc;

use jsonrpsee::core::middleware::{Batch, BatchEntry, Notification, RpcServiceT};
use jsonrpsee::types::error::{ErrorObject, TOO_BIG_BATCH_REQUEST_CODE, TOO_BIG_BATCH_REQUEST_MSG};
use jsonrpsee::types::{Id, Request};
use jsonrpsee::{BatchResponseBuilder, MethodResponse};
use reth_ethereum::rpc::builder::config::RethRpcServerConfig as _;
use reth_node_core::args::RpcServerArgs;
use thiserror::Error;
use tokio::sync::Semaphore;
use tower::Layer;

const DEFAULT_MAX_BATCH_REQUESTS: usize = 100;
const DEFAULT_CALL_PERMITS: usize = 32;
const DEFAULT_PROOF_PERMITS: usize = 16;
const DEFAULT_TRACE_PERMITS: usize = 16;

/// JSON-RPC error code for retryable resource exhaustion / limit exceeded (EIP-1474).
const RESOURCE_EXHAUSTED_ERROR_CODE: i32 = -32005;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeavyRpcClass {
    Call,
    Proof,
    Trace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
enum RpcAdmissionLimitError {
    #[error("RPC heavy call concurrency limit exceeded; retry shortly")]
    HeavyCall,
    #[error("RPC proof concurrency limit exceeded; retry shortly")]
    Proof,
    #[error("RPC trace concurrency limit exceeded; retry shortly")]
    Trace,
}

/// Configuration for [`OutbeRpcMiddlewareLayer`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutbeRpcProtectionConfig {
    /// Maximum number of entries allowed in a single JSON-RPC batch request.
    pub max_batch_requests: usize,
    /// Maximum serialized response size in bytes for a JSON-RPC batch response.
    pub max_response_size_bytes: u32,
    /// Fail-fast concurrency permits for heavy EVM call/log methods.
    pub call_permits: usize,
    /// Fail-fast concurrency permits for state/tribute proof methods.
    pub proof_permits: usize,
    /// Fail-fast concurrency permits for debug/trace/ots methods.
    pub trace_permits: usize,
}

impl Default for OutbeRpcProtectionConfig {
    fn default() -> Self {
        Self {
            max_batch_requests: DEFAULT_MAX_BATCH_REQUESTS,
            max_response_size_bytes: RpcServerArgs::default().rpc_max_response_size_bytes(),
            call_permits: DEFAULT_CALL_PERMITS,
            proof_permits: DEFAULT_PROOF_PERMITS,
            trace_permits: DEFAULT_TRACE_PERMITS,
        }
    }
}

impl OutbeRpcProtectionConfig {
    /// Overrides the batch response byte limit to match the node's parsed `--rpc.max-response-size`.
    #[must_use]
    pub const fn with_max_response_size_bytes(mut self, max_response_size_bytes: u32) -> Self {
        self.max_response_size_bytes = max_response_size_bytes;
        self
    }
}

#[derive(Debug)]
struct RpcProtectionState {
    max_batch_requests: usize,
    max_response_size_bytes: usize,
    call_gate: Arc<Semaphore>,
    proof_gate: Arc<Semaphore>,
    trace_gate: Arc<Semaphore>,
}

impl RpcProtectionState {
    fn new(config: OutbeRpcProtectionConfig) -> Self {
        Self {
            max_batch_requests: config.max_batch_requests,
            max_response_size_bytes: config.max_response_size_bytes as usize,
            call_gate: Arc::new(Semaphore::new(config.call_permits)),
            proof_gate: Arc::new(Semaphore::new(config.proof_permits)),
            trace_gate: Arc::new(Semaphore::new(config.trace_permits)),
        }
    }

    fn semaphore(&self, class: HeavyRpcClass) -> &Arc<Semaphore> {
        match class {
            HeavyRpcClass::Call => &self.call_gate,
            HeavyRpcClass::Proof => &self.proof_gate,
            HeavyRpcClass::Trace => &self.trace_gate,
        }
    }
}

/// Tower layer installing [`OutbeRpcMiddleware`] on Reth's HTTP and WebSocket RPC servers.
#[derive(Clone, Debug)]
pub struct OutbeRpcMiddlewareLayer {
    state: Arc<RpcProtectionState>,
}

impl OutbeRpcMiddlewareLayer {
    /// Creates a new RPC protection layer sharing concurrency gates across all connections.
    #[must_use]
    pub fn new(config: OutbeRpcProtectionConfig) -> Self {
        Self {
            state: Arc::new(RpcProtectionState::new(config)),
        }
    }
}

impl Default for OutbeRpcMiddlewareLayer {
    fn default() -> Self {
        Self::new(OutbeRpcProtectionConfig::default())
    }
}

impl<S> Layer<S> for OutbeRpcMiddlewareLayer {
    type Service = OutbeRpcMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        OutbeRpcMiddleware {
            inner,
            state: Arc::clone(&self.state),
        }
    }
}

/// JSON-RPC middleware enforcing batch size bounds and fail-fast heavy-method admission.
#[derive(Clone, Debug)]
pub struct OutbeRpcMiddleware<S> {
    inner: S,
    state: Arc<RpcProtectionState>,
}

fn classify_method(method: &str) -> Option<HeavyRpcClass> {
    match method {
        "eth_call"
        | "eth_callMany"
        | "eth_callBundle"
        | "eth_estimateGas"
        | "eth_createAccessList"
        | "eth_simulateV1"
        | "eth_getLogs"
        | "eth_getFilterLogs"
        | "eth_getFilterChanges"
        | "eth_getBlockReceipts" => Some(HeavyRpcClass::Call),
        "eth_getProof" => Some(HeavyRpcClass::Proof),
        _ if method.starts_with("debug_")
            || method.starts_with("trace_")
            || method.starts_with("ots_") =>
        {
            Some(HeavyRpcClass::Trace)
        }
        _ => None,
    }
}

fn admission_error_object(class: HeavyRpcClass) -> ErrorObject<'static> {
    let err = match class {
        HeavyRpcClass::Call => RpcAdmissionLimitError::HeavyCall,
        HeavyRpcClass::Proof => RpcAdmissionLimitError::Proof,
        HeavyRpcClass::Trace => RpcAdmissionLimitError::Trace,
    };
    ErrorObject::owned(RESOURCE_EXHAUSTED_ERROR_CODE, err.to_string(), None::<()>)
}

fn too_big_batch_error(max_batch_requests: usize) -> ErrorObject<'static> {
    ErrorObject::owned(
        TOO_BIG_BATCH_REQUEST_CODE,
        TOO_BIG_BATCH_REQUEST_MSG,
        Some(format!("Exceeded max limit of {max_batch_requests}")),
    )
}

impl<S> RpcServiceT for OutbeRpcMiddleware<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            NotificationResponse = MethodResponse,
            BatchResponse = MethodResponse,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = MethodResponse;
    type NotificationResponse = MethodResponse;
    type BatchResponse = MethodResponse;

    fn call<'a>(
        &self,
        request: Request<'a>,
    ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let inner = self.inner.clone();
        let state = Arc::clone(&self.state);
        async move {
            let Some(class) = classify_method(request.method_name()) else {
                return inner.call(request).await;
            };
            let Ok(permit) = state.semaphore(class).clone().try_acquire_owned() else {
                return MethodResponse::error(request.id, admission_error_object(class))
                    .with_extensions(request.extensions);
            };
            let _permit = permit;
            inner.call(request).await
        }
    }

    fn batch<'a>(
        &self,
        requests: Batch<'a>,
    ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        let service = self.clone();
        async move {
            let max_batch = service.state.max_batch_requests;
            if requests.len() > max_batch {
                return MethodResponse::error(Id::Null, too_big_batch_error(max_batch));
            }

            let mut batch_rp =
                BatchResponseBuilder::new_with_limit(service.state.max_response_size_bytes);
            let mut got_notification = false;

            for entry in requests.into_iter() {
                match entry {
                    Ok(BatchEntry::Call(req)) => {
                        let rp = service.call(req).await;
                        if let Err(err) = batch_rp.append(rp) {
                            return err;
                        }
                    }
                    Ok(BatchEntry::Notification(notif)) => {
                        got_notification = true;
                        service.notification(notif).await;
                    }
                    Err(err) => {
                        let (err_obj, id) = err.into_parts();
                        if let Err(err) = batch_rp.append(MethodResponse::error(id, err_obj)) {
                            return err;
                        }
                    }
                }
            }

            if batch_rp.is_empty() && got_notification {
                MethodResponse::notification()
            } else {
                MethodResponse::from_batch(batch_rp.finish())
            }
        }
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.inner.notification(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::core::client::ClientT;
    use jsonrpsee::core::middleware::RpcServiceBuilder;
    use jsonrpsee::core::params::BatchRequestBuilder;
    use jsonrpsee::server::{RpcModule, Server, ServerHandle};
    use jsonrpsee::types::error::{INVALID_REQUEST_CODE, TOO_BIG_BATCH_RESPONSE_CODE};
    use jsonrpsee::ws_client::WsClientBuilder;
    use serde_json::{json, Value};
    use std::net::SocketAddr;
    use tokio::sync::Notify;

    struct TestServer {
        addr: SocketAddr,
        _handle: ServerHandle,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    async fn spawn_test_server(config: OutbeRpcProtectionConfig) -> TestServer {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());

        let mut module = RpcModule::new(());
        module
            .register_method("eth_chainId", |_, _, _| "0x1")
            .expect("register eth_chainId");
        module
            .register_method("outbe_getFinalityProof", |_, _, _| "0xdeadbeef")
            .expect("register outbe_getFinalityProof");
        module
            .register_method("eth_large", |_, _, _| "x".repeat(256))
            .expect("register eth_large");

        let entered_call = Arc::clone(&entered);
        let release_call = Arc::clone(&release);
        module
            .register_async_method("eth_call", move |_, _, _| {
                let entered = Arc::clone(&entered_call);
                let release = Arc::clone(&release_call);
                async move {
                    entered.notify_one();
                    release.notified().await;
                    "0x42"
                }
            })
            .expect("register eth_call");

        let entered_proof = Arc::clone(&entered);
        let release_proof = Arc::clone(&release);
        module
            .register_async_method("eth_getProof", move |_, _, _| {
                let entered = Arc::clone(&entered_proof);
                let release = Arc::clone(&release_proof);
                async move {
                    entered.notify_one();
                    release.notified().await;
                    "0x99"
                }
            })
            .expect("register eth_getProof");

        let layer = OutbeRpcMiddlewareLayer::new(config);
        let server = Server::builder()
            .set_rpc_middleware(RpcServiceBuilder::new().layer(layer))
            .build("127.0.0.1:0")
            .await
            .expect("bind test rpc server");
        let addr = server.local_addr().expect("local_addr");
        let handle = server.start(module);

        TestServer {
            addr,
            _handle: handle,
            entered,
            release,
        }
    }

    async fn post_raw(addr: SocketAddr, body: &Value) -> (reqwest::StatusCode, String) {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}"))
            .json(body)
            .send()
            .await
            .expect("http post");
        let status = resp.status();
        let text = resp.text().await.expect("http body");
        (status, text)
    }

    #[tokio::test]
    async fn oversized_batch_rejected_with_too_big_batch_code_on_http_and_ws() {
        let srv = spawn_test_server(OutbeRpcProtectionConfig {
            max_batch_requests: 2,
            ..Default::default()
        })
        .await;

        let oversized = json!([
            {"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []},
            {"jsonrpc": "2.0", "id": 2, "method": "eth_chainId", "params": []},
            {"jsonrpc": "2.0", "id": 3, "method": "eth_chainId", "params": []},
        ]);
        let (_status, text) = post_raw(srv.addr, &oversized).await;
        let parsed: Value = serde_json::from_str(&text).expect("json error object");
        assert!(
            parsed.is_object(),
            "oversized batch must return single error object"
        );
        assert_eq!(parsed["id"], Value::Null);
        assert_eq!(parsed["error"]["code"], json!(TOO_BIG_BATCH_REQUEST_CODE));

        let ws_client = WsClientBuilder::default()
            .build(format!("ws://{}", srv.addr))
            .await
            .expect("ws connect");
        let mut batch = BatchRequestBuilder::new();
        for _ in 0..3 {
            batch
                .insert("eth_chainId", jsonrpsee::rpc_params![])
                .expect("insert");
        }
        let ws_err = ws_client
            .batch_request::<String>(batch)
            .await
            .expect_err("oversized ws batch must fail");
        let err_text = ws_err.to_string();
        assert!(
            err_text.contains("ID=null")
                || err_text.contains(&TOO_BIG_BATCH_REQUEST_CODE.to_string()),
            "expected Id::Null / -32010 batch rejection on ws, got: {err_text}"
        );
    }

    #[tokio::test]
    async fn shared_heavy_gate_rejects_standalone_and_batched_calls_across_http_and_ws() {
        let srv = spawn_test_server(OutbeRpcProtectionConfig {
            max_batch_requests: 4,
            call_permits: 1,
            proof_permits: 1,
            trace_permits: 1,
            ..Default::default()
        })
        .await;

        let ws_client = WsClientBuilder::default()
            .build(format!("ws://{}", srv.addr))
            .await
            .expect("ws connect");

        // Saturate the single call permit over WebSocket.
        let ws_in_flight = tokio::spawn({
            let ws_client = ws_client;
            async move {
                ws_client
                    .request::<String, _>("eth_call", jsonrpsee::rpc_params![])
                    .await
            }
        });
        srv.entered.notified().await;

        // Standalone HTTP heavy call fails fast with -32005.
        let single = json!({"jsonrpc": "2.0", "id": 10, "method": "eth_call", "params": []});
        let (_status, text) = post_raw(srv.addr, &single).await;
        let parsed: Value = serde_json::from_str(&text).expect("single error json");
        assert_eq!(parsed["id"], json!(10));
        assert_eq!(
            parsed["error"]["code"],
            json!(RESOURCE_EXHAUSTED_ERROR_CODE)
        );

        // Batched heavy call ALSO hits the same gate while lightweight eth_chainId succeeds.
        let batch = json!([
            {"jsonrpc": "2.0", "id": 11, "method": "eth_call", "params": []},
            {"jsonrpc": "2.0", "id": 12, "method": "eth_chainId", "params": []},
            {"jsonrpc": "2.0", "id": 13, "method": "outbe_getFinalityProof", "params": []},
        ]);
        let (_status, text) = post_raw(srv.addr, &batch).await;
        let arr: Vec<Value> = serde_json::from_str(&text).expect("batch response array");
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["id"], json!(11));
        assert_eq!(
            arr[0]["error"]["code"],
            json!(RESOURCE_EXHAUSTED_ERROR_CODE)
        );
        assert_eq!(arr[1]["id"], json!(12));
        assert_eq!(arr[1]["result"], json!("0x1"));
        assert_eq!(arr[2]["id"], json!(13));
        assert_eq!(arr[2]["result"], json!("0xdeadbeef"));

        // Release in-flight WS call and confirm permit is returned cleanly.
        srv.release.notify_waiters();
        let ws_res = ws_in_flight
            .await
            .expect("ws join")
            .expect("ws eth_call ok");
        assert_eq!(ws_res, "0x42");
    }

    #[tokio::test]
    async fn proof_gate_does_not_starve_follower_finality_proof() {
        let srv = spawn_test_server(OutbeRpcProtectionConfig {
            proof_permits: 1,
            ..Default::default()
        })
        .await;

        let addr = srv.addr;
        let in_flight = tokio::spawn(async move {
            post_raw(
                addr,
                &json!({"jsonrpc": "2.0", "id": 1, "method": "eth_getProof", "params": []}),
            )
            .await
        });
        srv.entered.notified().await;

        // Second eth_getProof fails fast with -32005, but outbe_getFinalityProof succeeds.
        let (_s, proof_text) = post_raw(
            srv.addr,
            &json!({"jsonrpc": "2.0", "id": 2, "method": "eth_getProof", "params": []}),
        )
        .await;
        let proof_json: Value = serde_json::from_str(&proof_text).expect("proof err");
        assert_eq!(
            proof_json["error"]["code"],
            json!(RESOURCE_EXHAUSTED_ERROR_CODE)
        );

        let (_s, finality_text) = post_raw(
            srv.addr,
            &json!({"jsonrpc": "2.0", "id": 3, "method": "outbe_getFinalityProof", "params": []}),
        )
        .await;
        let finality_json: Value = serde_json::from_str(&finality_text).expect("finality ok");
        assert_eq!(finality_json["result"], json!("0xdeadbeef"));

        srv.release.notify_waiters();
        let (_s, first_text) = in_flight.await.expect("join");
        let first_json: Value = serde_json::from_str(&first_text).expect("first ok");
        assert_eq!(first_json["result"], json!("0x99"));
    }

    #[tokio::test]
    async fn batch_preserves_jsonrpc_edge_cases_and_response_size_limit() {
        let srv = spawn_test_server(OutbeRpcProtectionConfig {
            max_batch_requests: 10,
            max_response_size_bytes: 300,
            ..Default::default()
        })
        .await;

        // Empty batch -> InvalidRequest (-32600).
        let (_s, empty_text) = post_raw(srv.addr, &json!([])).await;
        let empty_json: Value = serde_json::from_str(&empty_text).expect("empty batch err");
        assert_eq!(empty_json["error"]["code"], json!(INVALID_REQUEST_CODE));

        // Notification-only batch -> jsonrpsee returns 200 OK with MethodResponse::notification() ("null").
        let (notif_status, notif_text) = post_raw(
            srv.addr,
            &json!([{"jsonrpc": "2.0", "method": "eth_chainId", "params": []}]),
        )
        .await;
        assert_eq!(notif_status, reqwest::StatusCode::OK);
        assert_eq!(
            notif_text, "null",
            "notification-only batch must match jsonrpsee MethodResponse::notification()"
        );

        // Mixed batch with invalid entry -> error on invalid entry + result on valid entry.
        let (_s, mixed_text) = post_raw(
            srv.addr,
            &json!([
                {"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []},
                {"bad": true},
            ]),
        )
        .await;
        let mixed: Vec<Value> = serde_json::from_str(&mixed_text).expect("mixed batch array");
        assert_eq!(mixed.len(), 2);
        assert_eq!(mixed[0]["result"], json!("0x1"));
        assert_eq!(mixed[1]["error"]["code"], json!(INVALID_REQUEST_CODE));

        // Oversized batch response -> TOO_BIG_BATCH_RESPONSE_CODE (-32011).
        let (_s, big_text) = post_raw(
            srv.addr,
            &json!([
                {"jsonrpc": "2.0", "id": 1, "method": "eth_large", "params": []},
                {"jsonrpc": "2.0", "id": 2, "method": "eth_large", "params": []},
            ]),
        )
        .await;
        let big_json: Value = serde_json::from_str(&big_text).expect("big batch err");
        assert_eq!(
            big_json["error"]["code"],
            json!(TOO_BIG_BATCH_RESPONSE_CODE)
        );
    }
}
