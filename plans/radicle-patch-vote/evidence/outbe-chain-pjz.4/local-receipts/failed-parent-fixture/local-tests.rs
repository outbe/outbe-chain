use super::*;
use crate::manager::ManagerError;
use alloy_primitives::U256;
use outbe_primitives::radicle_publication::PublicationAction;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{mpsc, oneshot};

fn receipt() -> FinalizedPublicationReceipt {
    let body = PublicationBody {
        chain_id: 2026,
        genesis_hash: B256::repeat_byte(7),
        repo_id: [1; 20].into(),
        descriptor_hash: B256::repeat_byte(2),
        owner_bls_public: [3; 96].into(),
        players_commitment: B256::repeat_byte(4),
        epoch: 5,
        material_version: 6,
        material_hash: B256::repeat_byte(8),
        finalized_anchor_height: 0,
        finalized_anchor_hash: B256::repeat_byte(7),
        sequence: 1,
        previous_receipt_hash: B256::ZERO,
        action: PublicationAction::Bootstrap,
        proposal_id: U256::ZERO,
        patch_id: Default::default(),
        revision_id: Default::default(),
        expected_target_head: Default::default(),
        package_manifest_hash: B256::repeat_byte(9),
    };
    FinalizedPublicationReceipt {
        block: FinalizedBlock {
            number: 12,
            hash: B256::repeat_byte(12),
        },
        receipt: PublicationReceipt {
            accepted_height: 10,
            receipt_hash: body.receipt_hash(),
            body,
        },
    }
}

fn chain() -> ChainIdentity {
    ChainIdentity {
        chain_id: 2026,
        genesis_hash: B256::repeat_byte(7),
    }
}

#[derive(Default)]
struct Reader {
    calls: AtomicUsize,
    unavailable: AtomicBool,
}

impl RepositoryReceiptReader for Reader {
    fn read_current_publication_receipt(
        &self,
        sequence: u64,
    ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.unavailable.load(Ordering::SeqCst) {
            Err(ManagerError::Finality("test finality unavailable".into()))
        } else {
            Ok((sequence == 1).then(receipt))
        }
    }
}

#[tokio::test]
async fn actual_local_socket_returns_bound_receipts_without_fallback() {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    let control = directory.path().join("control");
    let reader = Arc::new(Reader::default());
    let server = LocalReceiptServer::bind(&control, reader.clone()).unwrap();
    let (stop, stopping) = oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopping.await;
    }));
    let client = LocalReceiptClient::new(&control, chain());
    assert_eq!(client.read(1).await.unwrap(), Some(receipt()));
    assert_eq!(client.read(0).await.unwrap(), None);
    assert_eq!(client.read(2).await.unwrap(), None);
    let mut wrong = chain();
    wrong.genesis_hash = B256::repeat_byte(99);
    assert!(matches!(
        LocalReceiptClient::new(&control, wrong).read(1).await,
        Err(Error::Protocol)
    ));
    reader.unavailable.store(true, Ordering::SeqCst);
    assert!(matches!(client.read(1).await, Err(Error::Unavailable)));
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert!(!receipt_socket(&control).exists());
    LocalReceiptServer::bind(&control, reader).unwrap();
}

#[tokio::test]
async fn private_permissions_symlinks_live_owners_and_stale_sockets_are_checked() {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    let control = directory.path().join("control");
    let path = receipt_socket(&control);
    let reader = Arc::new(Reader::default());
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        LocalReceiptServer::bind(&control, reader.clone()),
        Err(Error::Untrusted)
    ));
    assert_eq!(
        fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
        0o755
    );
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let target = directory.path().join("untouched");
    fs::write(&target, b"untouched").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(matches!(
        LocalReceiptServer::bind(&control, reader.clone()),
        Err(Error::Untrusted)
    ));
    assert_eq!(fs::read(&target).unwrap(), b"untouched");
    fs::remove_file(&path).unwrap();
    let stale = std::os::unix::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    drop(stale);
    let server = LocalReceiptServer::bind(&control, reader.clone()).unwrap();
    assert!(LocalReceiptServer::bind(&control, reader.clone()).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        LocalReceiptClient::new(&control, chain()).read(1).await,
        Err(Error::Untrusted)
    ));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::remove_file(&path).unwrap();
    let replacement = std::os::unix::net::UnixListener::bind(&path).unwrap();
    drop(server);
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_socket());
    drop(replacement);
}

#[tokio::test]
async fn malformed_oversized_and_arbitrary_signing_requests_never_reach_reader() {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    let control = directory.path().join("control");
    let reader = Arc::new(Reader::default());
    let server = LocalReceiptServer::bind(&control, reader.clone()).unwrap();
    let (stop, stopping) = oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopping.await;
    }));
    for bytes in [
        ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec(),
        [12_u32.to_be_bytes().as_slice(), b"SIGN", &[0; 8]].concat(),
        [4_u32.to_be_bytes().as_slice(), MAGIC].concat(),
    ] {
        let mut stream = UnixStream::connect(receipt_socket(&control)).await.unwrap();
        stream.write_all(&bytes).await.unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    }
    assert_eq!(reader.calls.load(Ordering::SeqCst), 0);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn client_deadline_covers_an_authenticated_but_unresponsive_peer() {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    let control = directory.path().join("control");
    let path = receipt_socket(&control);
    let listener = UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let client = LocalReceiptClient::new(&control, chain());
    let task = tokio::spawn(async move { client.read(1).await });
    let (_peer, _) = listener.accept().await.unwrap();
    tokio::time::pause();
    tokio::time::advance(DEADLINE).await;
    assert!(matches!(task.await.unwrap(), Err(Error::Deadline)));
}

struct BlockedReader {
    entered: mpsc::UnboundedSender<()>,
    release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl RepositoryReceiptReader for BlockedReader {
    fn read_current_publication_receipt(
        &self,
        _sequence: u64,
    ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError> {
        let _ = self.entered.send(());
        let (flag, condition) = &*self.release;
        let mut flag = flag.lock().unwrap();
        while !*flag {
            flag = condition.wait(flag).unwrap();
        }
        Ok(Some(receipt()))
    }
}

struct Release(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
impl Drop for Release {
    fn drop(&mut self) {
        *self.0 .0.lock().unwrap() = true;
        self.0 .1.notify_all();
    }
}

#[tokio::test]
async fn blocking_state_reads_are_bounded_without_an_unbounded_wait_queue() {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    let control = directory.path().join("control");
    let (entered, mut entries) = mpsc::unbounded_channel();
    let release = Release(Arc::new((
        std::sync::Mutex::new(false),
        std::sync::Condvar::new(),
    )));
    let reader = Arc::new(BlockedReader {
        entered,
        release: release.0.clone(),
    });
    let server = LocalReceiptServer::bind(&control, reader).unwrap();
    let (stop, stopping) = oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopping.await;
    }));
    let client = LocalReceiptClient::new(&control, chain());
    let mut requests = JoinSet::new();
    for _ in 0..MAX_READERS {
        let client = client.clone();
        requests.spawn(async move { client.read(1).await });
    }
    for _ in 0..MAX_READERS {
        tokio::time::timeout(Duration::from_secs(2), entries.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(matches!(client.read(1).await, Err(Error::Io(_))));
    assert!(entries.try_recv().is_err());
    drop(release);
    while let Some(result) = requests.join_next().await {
        assert_eq!(result.unwrap().unwrap(), Some(receipt()));
    }
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn response_decoder_checks_exact_request_chain_and_receipt_integrity() {
    let receipt = receipt();
    let mut response = MAGIC.to_vec();
    response.push(2);
    response.extend(receipt.block.number.to_be_bytes());
    response.extend(receipt.block.hash.0);
    response.extend(receipt.receipt.accepted_height.to_be_bytes());
    response.extend(receipt.receipt.receipt_hash.0);
    response.extend(receipt.receipt.body.to_bytes());
    assert_eq!(
        decode_response(&response, 1, chain()).unwrap(),
        Some(receipt)
    );
    assert!(decode_response(&response, 2, chain()).is_err());
    let mut zero_block = response.clone();
    zero_block[13..45].fill(0);
    assert!(decode_response(&zero_block, 1, chain()).is_err());
    for field in [45, 53, 85] {
        let mut invalid = response.clone();
        invalid[field] ^= 0xff;
        assert!(decode_response(&invalid, 1, chain()).is_err());
    }
    for invalid in [
        &response[..response.len() - 1],
        &[MAGIC.as_slice(), &[1, 0]].concat(),
    ] {
        assert!(decode_response(invalid, 1, chain()).is_err());
    }
}
