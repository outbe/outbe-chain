use super::*;
use crate::manager::ManagerError;
use alloy_primitives::U256;
use outbe_primitives::radicle_publication::PublicationAction;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{mpsc, oneshot};

fn private_directory() -> tempfile::TempDir {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

struct ProgressReader(std::sync::Mutex<crate::manager::FinalizedPublicationProgress>);
impl RepositoryReceiptReader for ProgressReader {
    fn read_current_publication_receipt(
        &self,
        _: u64,
    ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError> {
        Ok(None)
    }
    fn read_current_publication_progress(
        &self,
        _: &PublicationBody,
    ) -> Result<crate::manager::FinalizedPublicationProgress, ManagerError> {
        Ok(self.0.lock().unwrap().clone())
    }
}

#[tokio::test]
async fn publication_progress_socket_binds_exact_body_pending_indices_and_accepted_receipt() {
    use crate::manager::FinalizedPublicationProgress;
    let accepted = receipt();
    let body = accepted.receipt().body.clone();
    let pending = FinalizedPublicationProgress {
        block: accepted.block(),
        body_hash: body.receipt_hash(),
        attesters: (0..256).collect(),
        receipt: None,
    };
    let bytes = progress::encode(&pending, &body).unwrap();
    assert_eq!(bytes.len(), progress::MAX_FRAME);
    assert_eq!(progress::decode(&bytes, &body, chain()).unwrap(), pending);
    for malformed in [
        bytes[..4].to_vec(),
        bytes[..bytes.len() - 1].to_vec(),
        [bytes.clone(), vec![0]].concat(),
    ] {
        assert!(progress::decode(&malformed, &body, chain()).is_err());
    }
    let mut duplicate = bytes.clone();
    let last = duplicate.len() - 4;
    duplicate[last..].copy_from_slice(&254u32.to_be_bytes());
    assert!(progress::decode(&duplicate, &body, chain()).is_err());
    duplicate[last..].copy_from_slice(&256u32.to_be_bytes());
    assert!(progress::decode(&duplicate, &body, chain()).is_err());
    let mut other = body.clone();
    other.package_manifest_hash = B256::repeat_byte(99);
    assert!(progress::decode(&bytes, &other, chain()).is_err());
    let directory = private_directory();
    let control = directory.path().join("control");
    let reader = Arc::new(ProgressReader(std::sync::Mutex::new(pending.clone())));
    let server = LocalReceiptServer::bind(&control, reader.clone()).unwrap();
    let (stop, stopping) = oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopping.await;
    }));
    let client = LocalReceiptClient::new(&control, chain());
    assert_eq!(
        client.read_publication_progress(&body).await.unwrap(),
        pending
    );
    assert!(client.read_publication_progress(&other).await.is_err());
    *reader.0.lock().unwrap() = FinalizedPublicationProgress {
        block: accepted.block(),
        body_hash: body.receipt_hash(),
        attesters: vec![],
        receipt: Some(accepted.clone()),
    };
    let complete = client.read_publication_progress(&body).await.unwrap();
    assert_eq!(complete.receipt(), Some(&accepted));
    assert!(complete.attesters().is_empty());
    let mut wrong = chain();
    wrong.genesis_hash = B256::repeat_byte(99);
    assert!(LocalReceiptClient::new(&control, wrong)
        .read_publication_progress(&body)
        .await
        .is_err());
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

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
    let directory = private_directory();
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
    let directory = private_directory();
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
    assert!(LocalReceiptServer::bind(&control, reader.clone()).is_err());
    assert!(fs::symlink_metadata(&path).unwrap().file_type().is_socket());
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
    let directory = private_directory();
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
    let directory = private_directory();
    let control = directory.path().join("control");
    let path = receipt_socket(&control);
    let listener = UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let client = LocalReceiptClient::new(&control, chain());
    let task = tokio::spawn(async move { client.read(1).await });
    let (_peer, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
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
    let directory = private_directory();
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
    assert!(matches!(client.read(1).await, Err(Error::Unavailable)));
    assert!(entries.try_recv().is_err());
    tokio::time::pause();
    tokio::time::advance(DEADLINE + Duration::from_millis(1)).await;
    tokio::time::resume();
    while let Some(result) = requests.join_next().await {
        assert!(matches!(result.unwrap(), Err(Error::Deadline)));
    }
    // Timed-out clients must not free permits still held by blocking state readers.
    assert!(matches!(client.read(1).await, Err(Error::Unavailable)));
    assert!(entries.try_recv().is_err());
    drop(release);
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

fn signing_state() -> FinalizedPatchSigningState {
    signing_state_with_members(4)
}

fn signing_state_with_members(count: u64) -> FinalizedPatchSigningState {
    signing_fixture(count).0
}

pub(crate) fn signing_fixture(
    count: u64,
) -> (
    FinalizedPatchSigningState,
    commonware_cryptography::bls12381::dkg::feldman_desmedt::Output<
        commonware_cryptography::bls12381::primitives::variant::MinSig,
        commonware_cryptography::bls12381::PublicKey,
    >,
    Vec<commonware_cryptography::bls12381::PrivateKey>,
    Vec<commonware_cryptography::bls12381::primitives::group::Share>,
) {
    use crate::manager::{FinalizedPatchApproval, FinalizedRepositoryAuthority};
    use alloy_primitives::Address;
    use commonware_codec::Encode as _;
    use commonware_cryptography::{bls12381, Signer as _};
    use commonware_utils::TryCollect as _;
    use outbe_radicleregistry::{payload::PatchVotePayloadV1, PatchApprovalRecord};
    use outbe_validatorset::state::{CommitteeEntry, CommitteeSnapshot};

    let mut keys: Vec<_> = (201..201 + count)
        .map(bls12381::PrivateKey::from_seed)
        .collect();
    keys.sort_by_key(|key| key.public_key());
    let dkg = outbe_consensus::bls::bootstrap_dkg_for_participants(
        keys.iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap(),
    )
    .unwrap();
    let committee = CommitteeSnapshot {
        committee: keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let mut address = [0; 20];
                address[12..].copy_from_slice(&(index as u64 + 1).to_be_bytes());
                CommitteeEntry {
                    address: Address::from(address),
                    consensus_pubkey: key.public_key().encode().to_vec().try_into().unwrap(),
                }
            })
            .collect(),
        vrf_material_version: 6,
        vrf_group_public_key_bytes: dkg.polynomial.public().encode().to_vec(),
        vrf_public_polynomial_hash: outbe_consensus::dkg_manager::public_polynomial_hash(
            &dkg.polynomial,
        ),
    };
    let mut receipt = receipt();
    receipt.receipt.body.owner_bls_public =
        alloy_primitives::FixedBytes::from_slice(&committee.vrf_group_public_key_bytes);
    receipt.receipt.body.players_commitment =
        outbe_primitives::radicle_publication::players_commitment(
            committee
                .committee
                .iter()
                .map(|member| (member.address, &member.consensus_pubkey)),
        )
        .unwrap();
    receipt.receipt.receipt_hash = receipt.receipt.body.receipt_hash();
    let payload = PatchVotePayloadV1::accept(
        receipt.receipt.body.repo_id,
        [21; 20].into(),
        [22; 20].into(),
        [23; 20].into(),
    )
    .encode()
    .unwrap();
    let mut state = FinalizedPatchSigningState {
        authority: FinalizedRepositoryAuthority {
            block: receipt.block,
            chain_id: chain().chain_id,
            genesis_hash: chain().genesis_hash,
            epoch: 5,
            committee,
        },
        approval: FinalizedPatchApproval {
            block: receipt.block,
            approval: PatchApprovalRecord {
                proposal_id: U256::from(19),
                payload: String::from_utf8(payload).unwrap(),
                approved_height: 9,
                approved_timestamp: 1000,
            },
        },
        checkpoint: receipt.clone(),
        material: receipt,
        stage: outbe_radicleregistry::schema::PATCH_EXECUTION_PENDING,
        anchor: FinalizedBlock {
            number: 10,
            hash: B256::repeat_byte(10),
        },
    };
    let owner =
        radicle::crypto::BlsPublicKey::from_bytes(state.material.receipt.body.owner_bls_public.0)
            .unwrap();
    let material =
        RepositorySigningMaterial::new(state.authority.clone(), dkg.output.clone(), owner).unwrap();
    for receipt in [&mut state.checkpoint.receipt, &mut state.material.receipt] {
        receipt.body.material_hash = material.material_hash().into();
        receipt.receipt_hash = receipt.body.receipt_hash();
    }
    (state, dkg.output, keys, dkg.shares)
}

struct AuthorityReader {
    state: std::sync::Mutex<FinalizedPatchSigningState>,
    unavailable: AtomicBool,
    calls: AtomicUsize,
}

impl RepositoryAuthorityReader for AuthorityReader {
    fn read_current_repository_authority(
        &self,
    ) -> Result<crate::manager::FinalizedRepositoryAuthority, ManagerError> {
        panic!("the signing state must come from one read, not a separate authority sample")
    }

    fn read_current_patch_signing_state(
        &self,
        proposal_id: U256,
    ) -> Result<FinalizedPatchSigningState, ManagerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let state = self.state.lock().unwrap();
        if self.unavailable.load(Ordering::SeqCst)
            || proposal_id != state.approval.approval.proposal_id
        {
            return Err(ManagerError::Finality(
                "current signing state unavailable".into(),
            ));
        }
        Ok(state.clone())
    }
}

#[tokio::test]
async fn local_signing_state_is_requested_fresh_for_the_exact_proposal() {
    let directory = private_directory();
    let control = directory.path().join("control");
    let original = signing_state();
    let proposal = original.approval.approval.proposal_id;
    let authority = Arc::new(AuthorityReader {
        state: std::sync::Mutex::new(original.clone()),
        unavailable: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let receipts = Arc::new(Reader::default());
    let server = LocalReceiptServer::bind(&control, receipts.clone())
        .unwrap()
        .with_authority_reader(authority.clone());
    let (stop, stopping) = oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopping.await;
    }));
    let client = LocalReceiptClient::new(&control, chain());
    assert_eq!(
        client.read_patch_signing_state(proposal).await.unwrap(),
        original
    );
    assert!(matches!(
        client
            .read_patch_signing_state(proposal + U256::from(1))
            .await,
        Err(Error::Unavailable)
    ));
    {
        let mut state = authority.state.lock().unwrap();
        state.authority.block.number += 1;
        state.authority.block.hash = B256::repeat_byte(13);
        state.approval.block = state.authority.block;
        state.checkpoint.block = state.authority.block;
        state.material.block = state.authority.block;
        state.stage = outbe_radicleregistry::schema::PATCH_EXECUTION_REVIEWED;
    }
    let changed = authority.state.lock().unwrap().clone();
    assert_eq!(
        client.read_patch_signing_state(proposal).await.unwrap(),
        changed
    );
    assert!(matches!(
        LocalReceiptClient::new(
            &control,
            ChainIdentity {
                chain_id: chain().chain_id + 1,
                ..chain()
            }
        )
        .read_patch_signing_state(proposal)
        .await,
        Err(Error::Protocol)
    ));
    authority.unavailable.store(true, Ordering::SeqCst);
    assert!(matches!(
        client.read_patch_signing_state(proposal).await,
        Err(Error::Unavailable)
    ));
    assert_eq!(authority.calls.load(Ordering::SeqCst), 5);
    assert_eq!(receipts.calls.load(Ordering::SeqCst), 0);
    assert_eq!(client.read(1).await.unwrap(), Some(receipt()));
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    // A receipt-only server does not supply signing rights or a cached signing state.
    let server = LocalReceiptServer::bind(&control, receipts).unwrap();
    let (stop, stopping) = oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopping.await;
    }));
    assert!(matches!(
        client.read_patch_signing_state(proposal).await,
        Err(Error::Unavailable)
    ));
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[test]
fn signing_state_wire_rejects_mixed_contexts_bad_bindings_and_noncanonical_frames() {
    let original = signing_state();
    let proposal = original.approval.approval.proposal_id;
    let bytes = state::encode(&original).unwrap();
    assert_eq!(state::decode(&bytes, proposal, chain()).unwrap(), original);
    assert!(state::decode(&bytes, proposal + U256::from(1), chain()).is_err());
    for length in 0..bytes.len() {
        assert!(state::decode(&bytes[..length], proposal, chain()).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(state::decode(&trailing, proposal, chain()).is_err());
    let changes: &[fn(&mut FinalizedPatchSigningState)] = &[
        |s| s.authority.epoch += 1,
        |s| s.authority.committee.vrf_material_version += 1,
        |s| s.authority.committee.vrf_public_polynomial_hash = B256::ZERO,
        |s| s.authority.committee.committee.swap(0, 1),
        |s| s.authority.committee.committee[1] = s.authority.committee.committee[0].clone(),
        |s| s.approval.approval.approved_height = 99,
        |s| s.approval.approval.payload.push(' '),
        |s| s.checkpoint.receipt.receipt_hash = B256::ZERO,
        |s| s.material.receipt.accepted_height = 99,
        |s| s.stage = 0,
        |s| s.anchor.number += 1,
        |s| s.anchor.hash = B256::ZERO,
    ];
    for change in changes {
        let mut invalid = original.clone();
        change(&mut invalid);
        assert!(state::decode(&state::encode(&invalid).unwrap(), proposal, chain()).is_err());
    }
}

#[test]
fn signing_state_wire_supports_the_protocol_maximum_committee() {
    let current = signing_state_with_members(u64::from(outbe_consensus::bls::MAX_VALIDATORS));
    let proposal = current.approval.approval.proposal_id;
    let bytes = state::encode(&current).unwrap();
    assert!(bytes.len() <= state::MAX_FRAME);
    assert_eq!(state::decode(&bytes, proposal, chain()).unwrap(), current);
    let mut oversized = current.clone();
    oversized
        .authority
        .committee
        .committee
        .push(current.authority.committee.committee[0].clone());
    assert!(state::encode(&oversized).is_err());
    let mut oversized_count = bytes;
    // Header, finalized block, chain/genesis, epoch/version, polynomial hash, group key.
    let count_offset = 5 + 40 + 8 + 32 + 8 + 8 + 32 + 96;
    oversized_count[count_offset..count_offset + 2].copy_from_slice(&257_u16.to_be_bytes());
    assert!(state::decode(&oversized_count, proposal, chain()).is_err());
}

pub(crate) fn bootstrap_state(
    state: &FinalizedPatchSigningState,
) -> crate::manager::FinalizedBootstrapSigningState {
    crate::manager::FinalizedBootstrapSigningState {
        authority: state.authority.clone(),
        descriptor: outbe_primitives::radicle_genesis::GovernmentsDescriptor {
            chain_id: chain().chain_id,
            genesis_timestamp: 1700000000,
            description: "я".repeat(128),
            initial_tree_oid: "0x4b825dc642cb6eb9a060e54bf8d69288fbee4904"
                .parse()
                .unwrap(),
            initial_content_manifest_sha256:
                "0x4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
                    .parse()
                    .unwrap(),
        },
    }
}

struct ContextReader {
    state: std::sync::Mutex<Option<FinalizedPatchSigningState>>,
    authority: std::sync::Mutex<Option<crate::manager::FinalizedRepositoryAuthority>>,
}

impl RepositoryAuthorityReader for ContextReader {
    fn read_current_bootstrap_signing_state(
        &self,
    ) -> Result<crate::manager::FinalizedBootstrapSigningState, ManagerError> {
        let guard = self.state.lock().unwrap();
        let state = guard
            .as_ref()
            .ok_or_else(|| ManagerError::Finality("bootstrap unavailable".into()))?;
        Ok(bootstrap_state(state))
    }

    fn read_current_repository_authority(
        &self,
    ) -> Result<crate::manager::FinalizedRepositoryAuthority, ManagerError> {
        self.authority
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| ManagerError::Finality("authority unavailable".into()))
    }

    fn read_current_patch_signing_state(
        &self,
        proposal_id: U256,
    ) -> Result<FinalizedPatchSigningState, ManagerError> {
        self.state
            .lock()
            .unwrap()
            .as_ref()
            .filter(|state| state.approval.approval.proposal_id == proposal_id)
            .cloned()
            .ok_or_else(|| ManagerError::Finality("signing state unavailable".into()))
    }
}

#[tokio::test]
async fn public_output_crosses_the_socket_only_with_the_same_current_patch_context() {
    use crate::endpoint::EndpointAddress;
    use crate::integration::{
        network::shutdown_tests::TestReceiver, EndpointNetwork, EndpointSigningIdentity,
        EndpointTransport, LocalEndpointIdentity, LocalEndpointIdentityChannel,
        RadicleStatusChannel,
    };
    use commonware_codec::Encode as _;
    let directory = private_directory();
    let control = directory.path().join("control");
    let (original, output, keys, shares) = signing_fixture(4);
    let proposal = original.approval.approval.proposal_id;
    let validator = original.authority.committee.committee[0].address;
    let reader = Arc::new(ContextReader {
        state: std::sync::Mutex::new(Some(original.clone())),
        authority: std::sync::Mutex::new(Some(original.authority.clone())),
    });
    let (_, status) = RadicleStatusChannel::enabled(validator, [1; 32]);
    let (service, resolver, _) = EndpointNetwork::build(chain(), status);
    let mailbox = outbe_primitives::radicle_publication::RepositoryPublicationMailbox::default();
    let mut service = service
        .with_repository_authority_reader(reader.clone())
        .with_repository_publication_mailbox(mailbox.clone());
    let publisher = service.take_repository_material_publisher().unwrap();
    let (_, local) = LocalEndpointIdentityChannel::create(LocalEndpointIdentity {
        validator,
        node_id: [1; 32],
        addresses: vec![EndpointAddress::dns("local.example", 8776).unwrap()],
    });
    let (incoming, receiver) = mpsc::unbounded_channel();
    let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
    let actor = tokio::spawn(service.run(
        EndpointTransport {
            sender: RecordingSender {
                peer: None,
                sent: sent.clone(),
            },
            receiver: TestReceiver(receiver),
        },
        EndpointSigningIdentity {
            signer: keys[0].clone(),
            local,
        },
    ));
    let server = LocalReceiptServer::bind(&control, Arc::new(Reader::default()))
        .unwrap()
        .with_authority_reader(reader.clone())
        .with_material_resolver(resolver.clone())
        .with_publication_store(
            ReviewPackageStore::open(directory.path().join("packages")).unwrap(),
        );
    let publications = server.publications.as_ref().unwrap().clone();
    let (stop, stopping) = oneshot::channel();
    let task = tokio::spawn(server.run(async {
        let _ = stopping.await;
    }));
    let client = LocalReceiptClient::new(&control, chain());
    assert_eq!(
        LocalReceiptClient::connect(&control)
            .await
            .unwrap()
            .chain_identity(),
        chain()
    );
    assert!(matches!(
        client.read_patch_signing_context(proposal).await,
        Err(Error::Unavailable)
    ));
    assert!(client.read_bootstrap_signing_context().await.is_err());
    // No signing provider/share is installed: the channel returns public Output only.
    publisher.publish(5, 6, Some(&output));
    let bootstrap = client.read_bootstrap_signing_context().await.unwrap();
    assert_eq!(bootstrap.state, bootstrap_state(&original));
    assert_eq!(
        bootstrap.material.canonical_output(),
        output.encode().as_ref()
    );
    assert_eq!(bootstrap.state.descriptor().description.len(), 256);
    assert!(client.sign_bootstrap_document(&bootstrap).await.is_err());
    publisher.install_local_signer(outbe_consensus::hybrid::VrfMaterialProvider::new(
        6,
        output.public().clone(),
        Some(shares[0].clone()),
    ));
    let partial = client.sign_bootstrap_document(&bootstrap).await.unwrap();
    assert_eq!(partial.participant_index(), 0);
    use crate::integration::RepositoryGossipMessage;
    let own_message = RepositoryGossipMessage::Partial {
        ordinal: 0,
        frame: partial.clone(),
    };
    let wire = own_message.to_bytes().unwrap();
    assert_eq!(
        RepositoryGossipMessage::from_bytes(&wire)
            .unwrap()
            .to_bytes()
            .unwrap(),
        wire
    );
    for malformed in [
        wire[..5].to_vec(),
        wire[..wire.len() - 1].to_vec(),
        [wire.clone(), vec![0]].concat(),
    ] {
        assert!(RepositoryGossipMessage::from_bytes(&malformed).is_err());
    }
    assert_eq!(
        client
            .gossip_repository_message(&bootstrap.material, &own_message)
            .await
            .unwrap(),
        3
    );
    let deliveries = sent.lock().unwrap().clone();
    assert_eq!(deliveries.len(), 3);
    for ((peer, bytes), key) in deliveries.iter().zip(&keys[1..]) {
        assert_eq!(peer, &key.public_key());
        assert_eq!(bytes, &wire);
    }
    let mut foreign = chain();
    foreign.genesis_hash = B256::repeat_byte(99);
    assert!(LocalReceiptClient::new(&control, foreign)
        .sign_bootstrap_document(&bootstrap)
        .await
        .is_err());
    let wrong_request = [bootstrap::SIGN_DOCUMENT.as_slice(), &[0; 32]].concat();
    assert_eq!(
        client
            .exchange(&wrong_request, bootstrap::SIGN_FRAME)
            .await
            .unwrap(),
        [bootstrap::SIGN_DOCUMENT.as_slice(), &[0]].concat()
    );
    let mut changed = client.read_bootstrap_signing_context().await.unwrap();
    changed.state.descriptor.description = "Other".into();
    assert!(client.sign_bootstrap_document(&changed).await.is_err());

    use commonware_cryptography::Signer as _;
    use outbe_consensus::hybrid::VrfMaterialProvider;
    use radicle::storage::WriteRepository as _;
    let providers: Vec<_> = shares
        .iter()
        .map(|share| VrfMaterialProvider::new(6, output.public().clone(), Some(share.clone())))
        .collect();
    let document = crate::integration::bootstrap::prepare_bootstrap_document(
        bootstrap.state.descriptor(),
        &bootstrap.material,
    )
    .unwrap();
    let raw =
        radicle::git::raw::Repository::init_bare(directory.path().join("bootstrap.git")).unwrap();
    raw.treebuilder(None).unwrap().write().unwrap();
    let manifest = raw.blob(b"[]").unwrap().into();
    let wrong_manifest = raw.blob(b"wrong genesis content").unwrap().into();
    let repo =
        radicle::storage::git::Repository::open(raw.path(), document.repository_id()).unwrap();
    let mut contributions = vec![(keys[0].public_key(), partial)];
    for i in 1..3 {
        contributions.push((
            keys[i].public_key(),
            bootstrap
                .material
                .sign_bootstrap_document(
                    &providers[i],
                    &document,
                    &bootstrap.state,
                    original.authority.committee.committee[i].address,
                    &keys[i],
                )
                .unwrap(),
        ));
    }
    let document_frames = contributions
        .iter()
        .map(|(_, frame)| frame.clone())
        .collect::<Vec<_>>();
    let signature = bootstrap
        .material
        .recover_bootstrap_document(&document, &bootstrap.state, &contributions)
        .unwrap();
    let peer_message = RepositoryGossipMessage::Partial {
        ordinal: 0,
        frame: document_frames[1].clone(),
    };
    assert!(client
        .gossip_repository_message(&bootstrap.material, &peer_message)
        .await
        .is_err());
    let peer_wire = peer_message.to_bytes().unwrap();
    incoming
        .send((keys[2].public_key(), peer_wire.clone().into()))
        .unwrap();
    incoming
        .send((
            commonware_cryptography::bls12381::PrivateKey::from_seed(999).public_key(),
            peer_wire.clone().into(),
        ))
        .unwrap();
    let mut corrupted = peer_wire.clone();
    *corrupted.last_mut().unwrap() ^= 1;
    incoming
        .send((keys[1].public_key(), corrupted.into()))
        .unwrap();
    incoming
        .send((keys[1].public_key(), peer_wire.clone().into()))
        .unwrap();
    let messages = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let messages = client
                .take_repository_messages(&bootstrap.material)
                .await
                .unwrap();
            if !messages.is_empty() {
                break messages;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].sender, keys[1].public_key());
    assert_eq!(messages[0].message.to_bytes().unwrap(), peer_wire);
    assert!(client
        .take_repository_messages(&bootstrap.material)
        .await
        .unwrap()
        .is_empty());
    let identity = document.attach(&repo, &signature, b"[]").unwrap();
    assert!(client
        .sign_bootstrap_identity(&bootstrap, &identity, &repo, wrong_manifest)
        .await
        .is_err());
    let request = bootstrap::native_request(
        bootstrap::SIGN_IDENTITY,
        identity.body(),
        &repo,
        manifest,
        &[identity.document_signature()],
    )
    .unwrap();
    for malformed in [
        request[..request.len() - 1].to_vec(),
        [request.clone(), vec![0]].concat(),
    ] {
        assert_eq!(
            client
                .exchange(&malformed, bootstrap::SIGN_FRAME)
                .await
                .unwrap(),
            [bootstrap::SIGN_IDENTITY.as_slice(), &[0]].concat()
        );
    }
    let mut corrupt = request;
    *corrupt.last_mut().unwrap() ^= 1;
    assert_eq!(
        client
            .exchange(&corrupt, bootstrap::SIGN_FRAME)
            .await
            .unwrap(),
        [bootstrap::SIGN_IDENTITY.as_slice(), &[0]].concat()
    );
    let partial = client
        .sign_bootstrap_identity(&bootstrap, &identity, &repo, manifest)
        .await
        .unwrap();
    assert_eq!(partial.participant_index(), 0);
    assert!(bootstrap
        .material
        .verify_bootstrap_document_partial(
            &crate::integration::bootstrap::prepare_bootstrap_document(
                bootstrap.state.descriptor(),
                &bootstrap.material,
            )
            .unwrap(),
            &bootstrap.state,
            &keys[0].public_key(),
            &partial,
        )
        .is_err());
    let mut contributions = vec![(keys[0].public_key(), partial)];
    for i in 1..3 {
        contributions.push((
            keys[i].public_key(),
            bootstrap
                .material
                .sign_bootstrap_identity(
                    &providers[i],
                    &identity,
                    &bootstrap.state,
                    original.authority.committee.committee[i].address,
                    &keys[i],
                )
                .unwrap(),
        ));
    }
    let identity_frames = contributions
        .iter()
        .map(|(_, frame)| frame.clone())
        .collect::<Vec<_>>();
    let signature = bootstrap
        .material
        .recover_bootstrap_identity(&identity, &bootstrap.state, &contributions)
        .unwrap();
    let refs = identity.attach(&repo, &signature).unwrap();
    let partial = client
        .sign_bootstrap_refs(&bootstrap, &refs, &repo, manifest)
        .await
        .unwrap();
    assert_eq!(partial.participant_index(), 0);
    let mut contributions = vec![(keys[0].public_key(), partial)];
    for i in 1..3 {
        contributions.push((
            keys[i].public_key(),
            bootstrap
                .material
                .sign_bootstrap_refs(
                    &providers[i],
                    &refs,
                    &bootstrap.state,
                    original.authority.committee.committee[i].address,
                    &keys[i],
                )
                .unwrap(),
        ));
    }
    let refs_frames = contributions
        .iter()
        .map(|(_, frame)| frame.clone())
        .collect::<Vec<_>>();
    let signature = bootstrap
        .material
        .recover_bootstrap_refs(&refs, &bootstrap.state, &contributions)
        .unwrap();
    assert!(client
        .sign_bootstrap_refs(&changed, &refs, &repo, manifest)
        .await
        .is_err());
    *reader.state.lock().unwrap() = None;
    assert!(client.read_bootstrap_signing_context().await.is_err());
    assert!(client.sign_bootstrap_document(&bootstrap).await.is_err());
    assert!(client
        .sign_bootstrap_refs(&bootstrap, &refs, &repo, manifest)
        .await
        .is_err());
    let (body, package_manifest) = refs.attach(&repo, &signature).unwrap();
    assert_eq!(repo.raw().references().unwrap().count(), 0);
    *reader.state.lock().unwrap() = Some(original.clone());

    use crate::integration::{
        bootstrap::verify_bootstrap_publication, signing::witness::RepositoryWitnessBundle,
    };
    use outbe_primitives::radicle_publication::{
        PublicationAttestation, PublicationBatch, PUBLICATION_BODY_LEN, PUBLICATION_NAMESPACE,
    };
    let witness = RepositoryWitnessBundle::new(
        bootstrap.material.canonical_output(),
        vec![
            document_frames.clone(),
            identity_frames.clone(),
            refs_frames.clone(),
        ],
    )
    .unwrap();
    let package = verify_bootstrap_publication(
        &repo,
        &bootstrap.state,
        &bootstrap.material,
        b"[]",
        &body,
        &package_manifest,
        &witness,
    )
    .unwrap();
    let short = RepositoryWitnessBundle::new(
        bootstrap.material.canonical_output(),
        vec![document_frames[..2].to_vec(), identity_frames, refs_frames],
    )
    .unwrap();
    assert!(client
        .attest_bootstrap_publication(&bootstrap, &repo, manifest, &package, &short)
        .await
        .is_err());
    assert!(client
        .attest_bootstrap_publication(&bootstrap, &repo, wrong_manifest, &package, &witness)
        .await
        .is_err());
    let slot = crate::integration::publication::store::review_slot_key(&body);
    assert!(publications
        .lock()
        .unwrap()
        .load_selected(slot, &repo)
        .unwrap()
        .is_none());
    assert!(mailbox.ready_prefix(usize::MAX).is_none());
    let mut batch = PublicationBatch {
        body: body.clone(),
        attestations: vec![PublicationAttestation {
            participant_index: 0,
            signature: alloy_primitives::FixedBytes::from_slice(
                keys[0]
                    .sign(PUBLICATION_NAMESPACE, &body.to_bytes())
                    .encode()
                    .as_ref(),
            ),
        }],
    };
    for (i, key) in keys.iter().enumerate().take(3).skip(1) {
        batch.attestations.push(PublicationAttestation {
            participant_index: i as u32,
            signature: alloy_primitives::FixedBytes::from_slice(
                key.sign(PUBLICATION_NAMESPACE, &body.to_bytes())
                    .encode()
                    .as_ref(),
            ),
        });
    }
    let mut corrupt_batch = batch.clone();
    corrupt_batch.attestations[2].signature[0] ^= 1;
    assert!(client
        .queue_bootstrap_publication(&repo, manifest, &package, &witness, &corrupt_batch)
        .await
        .is_err());
    assert!(mailbox.ready_prefix(usize::MAX).is_none());
    assert!(publications
        .lock()
        .unwrap()
        .load_selected(slot, &repo)
        .unwrap()
        .is_none());
    let request = publication::request(
        publication::ATTEST,
        &repo,
        manifest,
        package.body(),
        package.manifest(),
        &witness,
    )
    .unwrap();
    for malformed in [
        request[..5].to_vec(),
        request[..request.len() - 1].to_vec(),
        [request.clone(), vec![0]].concat(),
    ] {
        assert_eq!(
            client
                .exchange(&malformed, publication::ATTEST_FRAME)
                .await
                .unwrap(),
            [publication::ATTEST.as_slice(), &[0]].concat()
        );
    }
    let attestation = client
        .attest_bootstrap_publication(&bootstrap, &repo, manifest, &package, &witness)
        .await
        .unwrap();
    assert_eq!(attestation.participant_index, 0);
    assert_eq!(attestation, batch.attestations[0]);
    let mut wrong_index = attestation.clone();
    wrong_index.participant_index = 4;
    assert!(bootstrap
        .material
        .verify_publication_attestation(&body, &wrong_index)
        .is_err());
    let mut wrong_generation = body.clone();
    wrong_generation.material_version += 1;
    assert!(bootstrap
        .material
        .verify_publication_attestation(&wrong_generation, &attestation)
        .is_err());
    let mut other_domain = attestation.clone();
    other_domain.signature = alloy_primitives::FixedBytes::from_slice(
        keys[0].sign(b"other", &body.to_bytes()).encode().as_ref(),
    );
    assert!(bootstrap
        .material
        .verify_publication_attestation(&body, &other_domain)
        .is_err());
    let selected = publications
        .lock()
        .unwrap()
        .load_selected(slot, &repo)
        .unwrap()
        .unwrap();
    assert_eq!(selected.body, body);
    assert_eq!(selected.manifest, package_manifest);
    let repeated = client
        .attest_bootstrap_publication(&bootstrap, &repo, manifest, &package, &witness)
        .await
        .unwrap();
    assert_eq!(repeated, attestation);
    let attestation_message = RepositoryGossipMessage::Attestation {
        body: body.clone(),
        attestation: attestation.clone(),
    };
    assert_eq!(
        client
            .gossip_repository_message(&bootstrap.material, &attestation_message)
            .await
            .unwrap(),
        3
    );

    client
        .queue_bootstrap_publication(&repo, manifest, &package, &witness, &batch)
        .await
        .unwrap();
    assert_eq!(mailbox.ready_prefix(usize::MAX), Some(batch.clone()));
    assert_eq!(
        mailbox
            .ready_prefix(PUBLICATION_BODY_LEN + 1 + 2 * 100)
            .unwrap()
            .attestations
            .len(),
        2
    );
    assert_eq!(repo.raw().references().unwrap().count(), 0);
    *reader.state.lock().unwrap() = None;
    assert!(client
        .attest_bootstrap_publication(&bootstrap, &repo, manifest, &package, &witness)
        .await
        .is_err());
    assert!(client
        .queue_bootstrap_publication(&repo, manifest, &package, &witness, &batch)
        .await
        .is_err());
    use crate::manager::EndpointResolver as _;
    resolver
        .refresh(&crate::manager::FinalizedSnapshot {
            block: original.authority.block,
            validators: vec![],
            registry_generation: 1,
            repositories: vec![],
        })
        .await
        .unwrap();
    assert!(mailbox.ready_prefix(usize::MAX).is_none());
    *reader.state.lock().unwrap() = Some(original.clone());
    client
        .queue_bootstrap_publication(&repo, manifest, &package, &witness, &batch)
        .await
        .unwrap();

    let (state, material) = client.read_patch_signing_context(proposal).await.unwrap();
    assert_eq!(state, original);
    assert_eq!(material.canonical_output(), output.encode().as_ref());
    assert_eq!(
        material.material_hash(),
        original.material.receipt.body.material_hash.0
    );
    assert!(matches!(
        client
            .read_patch_signing_context(proposal + U256::from(1))
            .await,
        Err(Error::Unavailable)
    ));
    let mut next = original.clone();
    next.authority.block.number += 1;
    next.authority.block.hash = B256::repeat_byte(13);
    next.approval.block = next.authority.block;
    next.checkpoint.block = next.authority.block;
    next.material.block = next.authority.block;
    // Actor observes a new finalized head between the state and Output reads.
    *reader.authority.lock().unwrap() = Some(next.authority.clone());
    assert!(client.read_bootstrap_signing_context().await.is_err());
    assert!(matches!(
        client.read_patch_signing_context(proposal).await,
        Err(Error::Unavailable)
    ));
    *reader.state.lock().unwrap() = Some(next.clone());
    assert_eq!(
        client.read_patch_signing_context(proposal).await.unwrap().0,
        next
    );
    // Same group key/output with a different material generation cannot pass old checkpoint binding.
    next.authority.epoch += 1;
    next.authority.committee.vrf_material_version += 1;
    *reader.authority.lock().unwrap() = Some(next.authority.clone());
    *reader.state.lock().unwrap() = Some(next.clone());
    publisher.publish(6, 7, Some(&output));
    wait_for_empty_mailbox(&mailbox).await;
    assert!(matches!(
        client.read_patch_signing_context(proposal).await,
        Err(Error::Unavailable)
    ));
    *reader.authority.lock().unwrap() = Some(original.authority.clone());
    *reader.state.lock().unwrap() = Some(original.clone());
    publisher.publish(5, 6, Some(&output));
    client
        .queue_bootstrap_publication(&repo, manifest, &package, &witness, &batch)
        .await
        .unwrap();
    publisher.publish(5, 6, None);
    wait_for_empty_mailbox(&mailbox).await;
    assert!(client.sign_bootstrap_document(&bootstrap).await.is_err());
    assert!(client.read_bootstrap_signing_context().await.is_err());
    assert!(matches!(
        client.read_patch_signing_context(proposal).await,
        Err(Error::Unavailable)
    ));
    publisher.publish(5, 6, Some(&output));
    assert!(client.read_patch_signing_context(proposal).await.is_ok());
    client
        .queue_bootstrap_publication(&repo, manifest, &package, &witness, &batch)
        .await
        .unwrap();
    *reader.state.lock().unwrap() = None;
    assert!(matches!(
        client.read_patch_signing_context(proposal).await,
        Err(Error::Unavailable)
    ));
    *reader.state.lock().unwrap() = Some(original);
    drop(publisher);
    wait_for_empty_mailbox(&mailbox).await;
    assert!(client.sign_bootstrap_document(&bootstrap).await.is_err());
    assert!(client.read_bootstrap_signing_context().await.is_err());
    assert!(matches!(
        client.read_patch_signing_context(proposal).await,
        Err(Error::Unavailable)
    ));
    resolver.shutdown().await.unwrap();
    actor.await.unwrap().unwrap();
    assert!(matches!(
        client.read_patch_signing_context(proposal).await,
        Err(Error::Unavailable)
    ));
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

async fn wait_for_empty_mailbox(
    mailbox: &outbe_primitives::radicle_publication::RepositoryPublicationMailbox,
) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while mailbox.ready_prefix(usize::MAX).is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn public_output_codec_binds_exact_material_and_rejects_corruption_and_lengths() {
    let (state, output, _, _) = signing_fixture(4);
    let proposal = state.approval.approval.proposal_id;
    let owner =
        radicle::crypto::BlsPublicKey::from_bytes(state.material.receipt.body.owner_bls_public.0)
            .unwrap();
    let material = RepositorySigningMaterial::new(state.authority.clone(), output, owner).unwrap();
    let bytes = material::encode(&state, &material).unwrap();
    let (decoded, decoded_material) = material::decode(&bytes, proposal, chain()).unwrap();
    assert_eq!(decoded, state);
    assert_eq!(
        decoded_material.canonical_output(),
        material.canonical_output()
    );
    assert!(material::decode(&bytes, proposal + U256::from(1), chain()).is_err());
    let mut wrong = chain();
    wrong.genesis_hash = B256::repeat_byte(99);
    assert!(material::decode(&bytes, proposal, wrong).is_err());
    for length in [0, 4, 5, 8, 9, bytes.len() - 1] {
        assert!(material::decode(&bytes[..length], proposal, chain()).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(material::decode(&trailing, proposal, chain()).is_err());
    let state_length = state::encode(&state).unwrap().len();
    let output_offset = 5 + 4 + state_length;
    for (offset, length) in [
        (5, state::MAX_FRAME + 1),
        (
            output_offset,
            crate::integration::signing::MAX_PUBLIC_OUTPUT_BYTES + 1,
        ),
    ] {
        let mut invalid = bytes.clone();
        invalid[offset..offset + 4].copy_from_slice(&(length as u32).to_be_bytes());
        assert!(material::decode(&invalid, proposal, chain()).is_err());
    }
    let mut corrupt_output = bytes.clone();
    let last = corrupt_output.len() - 1;
    corrupt_output[last] ^= 0xff;
    assert!(material::decode(&corrupt_output, proposal, chain()).is_err());
    let mut different = state.clone();
    different.material.receipt.body.material_hash = B256::repeat_byte(99);
    different.material.receipt.receipt_hash = different.material.receipt.body.receipt_hash();
    assert!(material::encode(&different, &material).is_err());
    // Keep the codec internally consistent but substitute a material hash not committed by Output.
    different.checkpoint = different.material.clone();
    let encoded = state::encode(&different).unwrap();
    assert_eq!(encoded.len(), state_length);
    let mut mixed = bytes;
    mixed[9..9 + state_length].copy_from_slice(&encoded);
    assert!(material::decode(&mixed, proposal, chain()).is_err());
}

fn preparation_manifest(
    state: &mut FinalizedPatchSigningState,
    output: &commonware_cryptography::bls12381::dkg::feldman_desmedt::Output<
        commonware_cryptography::bls12381::primitives::variant::MinSig,
        commonware_cryptography::bls12381::PublicKey,
    >,
    shares: &[commonware_cryptography::bls12381::primitives::group::Share],
) -> PublicationManifest {
    use crate::integration::publication::{
        native_policy_context, NativeSubject, ObjectCommitment, ObjectKind, OwnerRef, RefUpdate,
    };
    use commonware_codec::Encode as _;
    use outbe_consensus::hybrid::VrfMaterialProvider;
    use radicle::crypto::{BlsEnvelope, BlsPublicKey, RepositorySubject};
    let body = &state.checkpoint.receipt.body;
    let owner = BlsPublicKey::from_bytes(body.owner_bls_public.0).unwrap();
    let providers: Vec<_> = shares
        .iter()
        .map(|share| {
            VrfMaterialProvider::new(
                body.material_version,
                output.public().clone(),
                Some(share.clone()),
            )
        })
        .collect();
    let subjects = (0..3)
        .map(|ordinal| {
            let context = native_policy_context(body, ordinal);
            // Real threshold envelopes over framing fixtures, not a native Git bootstrap package.
            let subject = match ordinal {
                0 => RepositorySubject::identity(body.repo_id.0, context, [2; 20]),
                1 => RepositorySubject::cob(body.repo_id.0, context, [4; 20], b"fixture"),
                _ => RepositorySubject::signed_refs(body.repo_id.0, context, [0; 20], b"fixture")
                    .unwrap(),
            };
            let partials: Vec<_> = providers
                .iter()
                .enumerate()
                .take(3)
                .map(|(index, provider)| {
                    provider
                        .sign_repository(
                            body.material_version,
                            commonware_utils::Participant::from_usize(index),
                            output.public(),
                            &subject,
                        )
                        .unwrap()
                })
                .collect();
            let signature = providers[0]
                .recover_repository(body.material_version, output.public(), &subject, &partials)
                .unwrap();
            let envelope = BlsEnvelope::attach(
                subject,
                owner,
                signature.encode().as_ref().try_into().unwrap(),
            )
            .unwrap();
            NativeSubject {
                ordinal,
                envelope: envelope.to_hex(),
            }
        })
        .collect();
    let owner_snapshot = vec![
        OwnerRef {
            name: "refs/heads/main".into(),
            oid: hex::encode([1; 20]),
        },
        OwnerRef {
            name: "refs/rad/root".into(),
            oid: hex::encode([2; 20]),
        },
    ];
    let mut refs: Vec<_> = owner_snapshot
        .iter()
        .map(|row| RefUpdate {
            name: row.name.clone(),
            old: hex::encode([0; 20]),
            new: row.oid.clone(),
        })
        .collect();
    refs.push(RefUpdate {
        name: "refs/rad/sigrefs".into(),
        old: hex::encode([0; 20]),
        new: hex::encode([3; 20]),
    });
    let manifest = PublicationManifest::new(
        vec![ObjectCommitment {
            kind: ObjectKind::Commit,
            oid: hex::encode([3; 20]),
            sha256: hex::encode([4; 32]),
        }],
        refs,
        subjects,
        owner_snapshot,
    )
    .unwrap();
    for receipt in [&mut state.checkpoint.receipt, &mut state.material.receipt] {
        receipt.body.package_manifest_hash = manifest.hash().unwrap();
        receipt.receipt_hash = receipt.body.receipt_hash();
    }
    manifest
}

struct PreparationReceipts {
    current: std::sync::Mutex<Option<FinalizedPublicationReceipt>>,
}

impl RepositoryReceiptReader for PreparationReceipts {
    fn read_current_publication_receipt(
        &self,
        sequence: u64,
    ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError> {
        Ok(self
            .current
            .lock()
            .unwrap()
            .as_ref()
            .filter(|r| r.receipt().body.sequence == sequence)
            .cloned())
    }
}

#[tokio::test]
async fn preparation_socket_verifies_manifest_receipts_and_replays_after_restart() {
    use crate::endpoint::EndpointAddress;
    use crate::integration::{
        network::shutdown_tests::{NoSend, TestReceiver},
        EndpointNetwork, EndpointSigningIdentity, EndpointTransport, LocalEndpointIdentity,
        LocalEndpointIdentityChannel, RadicleStatusChannel,
    };
    let directory = private_directory();
    let control = directory.path().join("control");
    let store_path = publication_store_path(&control);
    let (mut original, output, keys, shares) = signing_fixture(4);
    let manifest = preparation_manifest(&mut original, &output, &shares);
    let proposal = original.approval.approval.proposal_id;
    let receipts = Arc::new(PreparationReceipts {
        current: std::sync::Mutex::new(Some(original.checkpoint.clone())),
    });
    let reader = Arc::new(ContextReader {
        state: std::sync::Mutex::new(Some(original.clone())),
        authority: std::sync::Mutex::new(Some(original.authority.clone())),
    });
    let validator = original.authority.committee.committee[0].address;
    let (_, status) = RadicleStatusChannel::enabled(validator, [1; 32]);
    let (service, resolver, _) = EndpointNetwork::build(chain(), status);
    let mut service = service.with_repository_authority_reader(reader.clone());
    let publisher = service.take_repository_material_publisher().unwrap();
    publisher.publish(5, 6, Some(&output));
    let (_, local) = LocalEndpointIdentityChannel::create(LocalEndpointIdentity {
        validator,
        node_id: [1; 32],
        addresses: vec![EndpointAddress::dns("local.example", 8776).unwrap()],
    });
    let (_incoming, receiver) = mpsc::unbounded_channel();
    let actor = tokio::spawn(service.run(
        EndpointTransport {
            sender: NoSend,
            receiver: TestReceiver(receiver),
        },
        EndpointSigningIdentity {
            signer: keys[0].clone(),
            local,
        },
    ));
    let client = LocalReceiptClient::new(&control, chain());
    for restart in [false, true] {
        let store = ReviewPackageStore::open(&store_path).unwrap();
        let server = LocalReceiptServer::bind(&control, receipts.clone())
            .unwrap()
            .with_authority_reader(reader.clone())
            .with_material_resolver(resolver.clone())
            .with_publication_store(store);
        let (stop, stopping) = oneshot::channel();
        let task = tokio::spawn(server.run(async {
            let _ = stopping.await;
        }));
        if !restart {
            assert!(matches!(
                client.read_patch_preparation_context(proposal).await,
                Err(Error::Unavailable)
            ));
            let receipt = client.read(1).await.unwrap().unwrap();
            client
                .cache_checkpoint_manifest(&receipt, &manifest)
                .await
                .unwrap();
            client
                .cache_checkpoint_manifest(&receipt, &manifest)
                .await
                .unwrap();
            let wrong_manifest = PublicationManifest::new(
                vec![],
                vec![],
                vec![],
                manifest.owner_snapshot().to_vec(),
            )
            .unwrap();
            assert!(client
                .cache_checkpoint_manifest(&receipt, &wrong_manifest)
                .await
                .is_err());
            let mut wrong_receipt = receipt.clone();
            wrong_receipt.receipt.receipt_hash = B256::repeat_byte(99);
            assert!(client
                .cache_checkpoint_manifest(&wrong_receipt, &manifest)
                .await
                .is_err());
            let mut wrong_chain = chain();
            wrong_chain.chain_id += 1;
            assert!(matches!(
                LocalReceiptClient::new(&control, wrong_chain)
                    .cache_checkpoint_manifest(&receipt, &manifest)
                    .await,
                Err(Error::Protocol)
            ));
        }
        let prepared = client
            .read_patch_preparation_context(proposal)
            .await
            .unwrap();
        assert_eq!(prepared.state, original);
        assert_eq!(
            prepared.material.material_hash(),
            original.material.receipt.body.material_hash.0
        );
        assert_eq!(
            prepared.snapshot.owner_refs(),
            &manifest.owner_refs().unwrap()
        );
        assert_eq!(
            prepared.snapshot.sigrefs().to_string(),
            hex::encode([3; 20])
        );
        *receipts.current.lock().unwrap() = None;
        assert!(matches!(
            client.read_patch_preparation_context(proposal).await,
            Err(Error::Unavailable)
        ));
        *receipts.current.lock().unwrap() = Some(original.checkpoint.clone());
        if restart {
            let path = store_path
                .join("packages")
                .join(hex::encode(original.checkpoint.receipt.receipt_hash))
                .join("manifest");
            fs::write(&path, b"{}").unwrap();
            assert!(matches!(
                client.read_patch_preparation_context(proposal).await,
                Err(Error::Unavailable)
            ));
            fs::write(path, manifest.to_bytes().unwrap()).unwrap();
            publisher.publish(5, 6, None);
            assert!(matches!(
                client.read_patch_preparation_context(proposal).await,
                Err(Error::Unavailable)
            ));
        }
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    }
    resolver.shutdown().await.unwrap();
    actor.await.unwrap().unwrap();
}

#[test]
fn preparation_context_codec_requires_exact_context_refs_and_sigrefs() {
    use crate::integration::publication::CheckpointOwnerSnapshot;
    use commonware_codec::Encode as _;
    let (mut state, output, _, shares) = signing_fixture(4);
    let manifest = preparation_manifest(&mut state, &output, &shares);
    let proposal = state.approval.approval.proposal_id;
    let owner =
        radicle::crypto::BlsPublicKey::from_bytes(state.material.receipt.body.owner_bls_public.0)
            .unwrap();
    let material = RepositorySigningMaterial::from_canonical_output(
        state.authority.clone(),
        &output.encode(),
        owner,
    )
    .unwrap();
    let snapshot = CheckpointOwnerSnapshot::bootstrap(state.checkpoint.clone(), &manifest).unwrap();
    let bytes = preparation::encode(&state, &material, &snapshot).unwrap();
    let decoded = preparation::decode(&bytes, proposal, chain()).unwrap();
    assert_eq!(decoded.snapshot.owner_refs(), snapshot.owner_refs());
    assert_eq!(decoded.snapshot.sigrefs(), snapshot.sigrefs());
    assert_eq!(decoded.snapshot.checkpoint(), state.checkpoint());
    for length in [0, 4, 5, 8, 9, bytes.len() - 1] {
        assert!(preparation::decode(&bytes[..length], proposal, chain()).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(preparation::decode(&trailing, proposal, chain()).is_err());
    let mut zero = bytes.clone();
    let length = zero.len();
    zero[length - 20..].fill(0);
    assert!(preparation::decode(&zero, proposal, chain()).is_err());
    let mut oversized = bytes;
    oversized[5..9].copy_from_slice(&((material::MAX_FRAME + 1) as u32).to_be_bytes());
    assert!(preparation::decode(&oversized, proposal, chain()).is_err());
    assert!(CheckpointOwnerSnapshot::from_local_refs(
        state.checkpoint.clone(),
        Default::default(),
        snapshot.sigrefs()
    )
    .is_err());
}

#[test]
fn bootstrap_context_codec_binds_public_material_descriptor_and_exact_chain() {
    let (patch, output, _, _) = signing_fixture(4);
    let state = bootstrap_state(&patch);
    let owner =
        radicle::crypto::BlsPublicKey::from_bytes(patch.material.receipt.body.owner_bls_public.0)
            .unwrap();
    let material =
        RepositorySigningMaterial::new(state.authority().clone(), output, owner).unwrap();
    let bytes = bootstrap::encode(&state, &material).unwrap();
    let decoded = bootstrap::decode(&bytes, chain()).unwrap();
    assert_eq!(decoded.state, state);
    assert_eq!(
        decoded.material.canonical_output(),
        material.canonical_output()
    );
    let mut wrong = chain();
    wrong.genesis_hash = B256::repeat_byte(99);
    assert!(bootstrap::decode(&bytes, wrong).is_err());
    for length in [0, 4, 5, 8, bytes.len() - 1] {
        assert!(bootstrap::decode(&bytes[..length], chain()).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(bootstrap::decode(&trailing, chain()).is_err());
    let offset = 5 + state::encode_authority(state.authority()).unwrap().len();
    let mut oversized = bytes.clone();
    oversized[offset..offset + 2].copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(bootstrap::decode(&oversized, chain()).is_err());
    let mut corrupt = bytes.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 255;
    assert!(bootstrap::decode(&corrupt, chain()).is_err());
    let mut changed = state.clone();
    changed.authority.block.number += 1;
    assert!(bootstrap::encode(&changed, &material).is_err());
    assert!(bootstrap::decode(&vec![0; bootstrap::MAX_FRAME + 1], chain()).is_err());
}

#[derive(Clone)]
struct RecordingSender {
    peer: Option<commonware_cryptography::bls12381::PublicKey>,
    sent: Arc<std::sync::Mutex<Vec<RecordedDelivery>>>,
}

type RecordedDelivery = (commonware_cryptography::bls12381::PublicKey, Vec<u8>);

impl commonware_p2p::LimitedSender for RecordingSender {
    type PublicKey = commonware_cryptography::bls12381::PublicKey;
    type Checked<'a> = Self;
    fn check(
        &mut self,
        recipients: commonware_p2p::Recipients<Self::PublicKey>,
    ) -> Result<Self, std::time::SystemTime> {
        let commonware_p2p::Recipients::One(peer) = recipients else {
            return Err(std::time::SystemTime::now());
        };
        Ok(Self {
            peer: Some(peer),
            sent: self.sent.clone(),
        })
    }
}

impl commonware_p2p::CheckedSender for RecordingSender {
    type PublicKey = commonware_cryptography::bls12381::PublicKey;
    fn recipients(&self) -> Vec<Self::PublicKey> {
        self.peer.iter().cloned().collect()
    }
    fn send(
        self,
        message: impl Into<commonware_runtime::IoBufs> + Send,
        _: bool,
    ) -> commonware_actor::Unreliable<commonware_actor::Feedback> {
        use bytes::Buf as _;
        let mut bytes = message.into();
        let bytes = bytes.copy_to_bytes(bytes.remaining()).to_vec();
        self.sent.lock().unwrap().push((self.peer.unwrap(), bytes));
        commonware_actor::Unreliable::Outcome(commonware_actor::Feedback::Ok)
    }
}
