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
        call(-32005),
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

fn hex_bytes(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn proof(finalization_hex: &str, block_hex: &str) -> super::UpstreamFinalizationProof {
    super::UpstreamFinalizationProof {
        finalization_hex: finalization_hex.to_string(),
        block_hex: block_hex.to_string(),
    }
}

fn sample_block() -> outbe_consensus::block::ConsensusBlock {
    use outbe_consensus::block::ConsensusBlock;
    use outbe_primitives::OutbeHeader;
    use reth_ethereum::primitives::SealedBlock;
    use reth_ethereum::Block;

    let mut raw = Block::default();
    raw.header.number = 11;
    raw.header.extra_data = alloy_primitives::Bytes::from_static(b"w05");
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(raw.map_header(OutbeHeader::new)))
}

/// A real committee certificate and the block it finalizes, both inside
/// `MAX_COMMITTEE_MEMBERS`.
fn certified_record() -> (
    Vec<u8>,
    Vec<u8>,
    outbe_consensus::marshal_types::Finalization,
    outbe_consensus::block::ConsensusBlock,
) {
    use commonware_codec::Encode as _;
    use commonware_consensus::simplex::types::{Finalization, Finalize, Proposal};
    use commonware_consensus::types::{Epoch, Round, View};
    use commonware_cryptography::bls12381::{self, primitives::variant::MinSig};
    use commonware_cryptography::Signer as _;
    use commonware_parallel::Sequential;
    use commonware_utils::TryCollect as _;
    use outbe_consensus::hybrid::HybridScheme;

    let keys: Vec<bls12381::PrivateKey> = (1u64..=4).map(bls12381::PrivateKey::from_seed).collect();
    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .expect("participant set");
    let dkg = outbe_consensus::bls::bootstrap_dkg(4).expect("dkg");
    let signers: Vec<HybridScheme<MinSig>> = keys
        .iter()
        .map(|key| {
            let index = participants
                .iter()
                .position(|pk| pk == &key.public_key())
                .expect("index");
            HybridScheme::signer(
                &outbe_consensus::config::outbe_app_namespace(),
                participants.clone(),
                key.clone(),
                dkg.polynomial.clone(),
                dkg.shares[index].clone(),
            )
            .expect("signer")
        })
        .collect();
    let verifier = HybridScheme::<MinSig>::verifier(
        &outbe_consensus::config::outbe_app_namespace(),
        participants,
        dkg.polynomial.clone(),
    )
    .expect("verifier");
    let block = sample_block();
    let proposal = Proposal::new(
        Round::new(Epoch::new(1), View::new(2)),
        View::new(1),
        block.digest(),
    );
    let votes: Vec<_> = signers
        .iter()
        .map(|scheme| Finalize::sign(scheme, proposal.clone()).expect("vote"))
        .collect();
    let finalization = Finalization::from_finalizes(
        &verifier,
        commonware_utils::iter::NonEmpty::try_new(votes.iter()).expect("votes"),
        &Sequential,
    )
    .expect("certificate");
    (
        finalization.encode().to_vec(),
        block.encode().to_vec(),
        finalization,
        block,
    )
}

/// Finalization bytes whose signer bitmap declares 257 participants.
///
/// The bitmap length is rejected before the signature bytes are read, so the
/// fixture is the proposal plus that bitmap.
fn oversized_finalization_bytes() -> Vec<u8> {
    use commonware_codec::Encode as _;
    use commonware_consensus::simplex::types::Proposal;
    use commonware_consensus::types::{Epoch, Round, View};
    use commonware_cryptography::certificate::Signers;
    use commonware_utils::Participant;
    use outbe_consensus::digest::Digest;

    let participants =
        u32::try_from(outbe_consensus::bls::MAX_COMMITTEE_MEMBERS + 1).expect("257 fits in u32");
    let signers = Signers::new(participants, [Participant::new(0)]).expect("signer bitmap");
    let proposal = Proposal::new(
        Round::new(Epoch::new(1), View::new(1)),
        View::new(0),
        Digest::ZERO,
    );
    let mut bytes = proposal.encode().to_vec();
    bytes.extend_from_slice(signers.encode().as_ref());
    bytes
}

#[test]
fn decode_finalization_proof_rejects_malformed_hex_and_trailing_bytes() {
    let (finalization, block, _, _) = certified_record();
    let finalization_hex = hex_bytes(&finalization);
    let block_hex = hex_bytes(&block);
    let rejected = [
        proof("zz", &block_hex),
        proof("0xzz", &block_hex),
        proof("abc", &block_hex),
        proof("0xabc", &block_hex),
        proof("", &block_hex),
        proof(&finalization_hex, "zz"),
        proof(&finalization_hex, "0xzz"),
        proof(&finalization_hex, "abc"),
        proof(&finalization_hex, "0xabc"),
        proof(&finalization_hex, ""),
        proof(&format!("{finalization_hex}00"), &block_hex),
        proof(&finalization_hex, &format!("{block_hex}00")),
    ];
    for case in &rejected {
        assert!(super::decode_finalization_proof(case).is_none());
    }
}

#[test]
fn decode_rejects_oversized_upstream_proof() {
    let finalization = oversized_finalization_bytes();
    let block = {
        use commonware_codec::Encode as _;
        sample_block().encode().to_vec()
    };
    assert_eq!(
        outbe_consensus::bls::MAX_COMMITTEE_MEMBERS + 1,
        257,
        "the codec bound under test is 256 participants"
    );
    assert!(matches!(
        outbe_consensus::follow::decode_public_finalization(
            &finalization,
            outbe_consensus::bls::MAX_COMMITTEE_MEMBERS,
        ),
        Err(outbe_consensus::follow::PublicFinalizedBlockDecodeError::Finalization(_))
    ));
    assert!(super::decode_finalization_proof(&proof(
        &hex_bytes(&finalization),
        &hex_bytes(&block)
    ))
    .is_none());
}

#[test]
fn well_formed_proof_still_decodes() {
    use commonware_codec::Encode as _;

    let (finalization_bytes, block_bytes, finalization, block) = certified_record();
    let decoded = super::decode_finalization_proof(&proof(
        &hex_bytes(&finalization_bytes),
        &hex_bytes(&block_bytes),
    ))
    .expect("a certificate inside MAX_COMMITTEE_MEMBERS decodes");
    assert_eq!(decoded.finalization.encode(), finalization.encode());
    assert_eq!(decoded.block.encode(), block.encode());
}
