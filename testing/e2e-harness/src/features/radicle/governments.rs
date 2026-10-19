//! Production genesis, real Vote transactions, automatic signing/publication and native fetch.

use super::*;
use alloy_primitives::U256;
use eyre::{ensure, eyre, Result};
use outbe_coderegistry::payload::PatchVotePayloadV1;
use outbe_primitives::{
    radicle_genesis::GovernmentsDescriptor, radicle_publication::PublicationAction,
};
use outbe_radicle::{
    integration::{
        bootstrap::verify_accepted_bootstrap,
        local::LocalReceiptClient,
        publication::{
            binding::verify_accepted_material_publication,
            merge::verify_accepted_merge_publication, policy::AcceptedRepositoryPolicy,
            review::verify_accepted_review_publication, CheckpointOwnerSnapshot,
            PublicationManifest,
        },
        signing::accepted::AcceptedPublicationVerifier,
    },
    manager::FinalizedPublicationReceipt,
};
use radicle::{
    cob::{patch, store::access::ReadOnly},
    crypto::{BlsPublicKey, RepositoryKey},
    git,
    identity::RepoId,
    storage::{git::Repository, ReadRepository as _, ReadStorage as _},
};
use std::{collections::BTreeMap, fs, path::Path, sync::Arc};

use crate::{internal::eth, world::state::GovernmentsPatchEvidenceV1};

// Keep at least 64 publication blocks before the shared 15-block DKG preparation window.
const GOVERNMENTS_DKG_EPOCH_BLOCKS: u64 = 96;

#[given("a fresh four-validator governments patch-vote localnet")]
fn fresh(world: &mut World) {
    assert_eq!(world.validators.size(), 4);
    let profile = BootstrapProfile::default()
        .with_dkg_timing(
            GOVERNMENTS_DKG_EPOCH_BLOCKS,
            RADICLE_DKG_PREPARE_BLOCKS,
            RADICLE_DKG_ACTIVATION_GRACE_BLOCKS,
        )
        .unwrap()
        .with_governance_voting_window(24)
        .unwrap()
        .with_governments()
        .unwrap();
    world.state.voting_window = 24;
    world.state.wwd = Some(worldwide_day());
    world
        .localnet
        .bootstrap_with_profile(4, &profile)
        .expect("governments production genesis");
    start_bootstrapped_localnet(world, &StartOpts::with_voting_window(24));
}

#[then("the governments validators form the signed mesh")]
fn mesh(world: &mut World) {
    observe_mesh(world);
}

#[when("an independent author publishes a governments patch")]
fn propose_patch(world: &mut World) {
    let rid = repository_id(world);
    world.state.radicle.governments_patch = Some(
        world
            .localnet
            .publish_governments_patch(rid)
            .expect("ordinary Ed25519 author patch"),
    );
}

#[then("every validator sees the exact author revision while governments main remains unchanged")]
fn before_vote(world: &mut World) {
    let patch = patch_evidence(world);
    for index in 0..4 {
        wait_observation("Ed25519 patch reaches validator before Vote", || {
            let (repo, _, history) = verified_history(world, index, None)?;
            ensure!(
                history
                    .iter()
                    .all(|r| r["action"] != "Review" && r["action"] != "Merge"),
                "owner acted before Vote"
            );
            ensure!(
                repo.head()?.1.to_string() == patch.base,
                "network main changed before Vote"
            );
            let store = patch::Patches::open(&repo, ReadOnly)?;
            let proposed = store
                .get(&patch.patch_id.parse()?)?
                .ok_or_else(|| eyre!("patch not fetched yet"))?;
            assert_patch_revision(&proposed, &patch)?;
            ensure!(proposed.is_open(), "patch merged before Vote");
            Ok(())
        });
    }
}

#[when("the current validators vote to accept that governments revision")]
fn vote(world: &mut World) {
    let patch = patch_evidence(world);
    let decision = PatchVotePayloadV1::accept(
        repository_id(world).into_sha1().unwrap().into(),
        patch
            .patch_id
            .parse::<git::Oid>()
            .unwrap()
            .into_sha1()
            .unwrap()
            .into(),
        patch
            .revision_id
            .parse::<git::Oid>()
            .unwrap()
            .into_sha1()
            .unwrap()
            .into(),
        patch
            .base
            .parse::<git::Oid>()
            .unwrap()
            .into_sha1()
            .unwrap()
            .into(),
    );
    let payload = String::from_utf8(decision.encode().unwrap()).unwrap();
    let port = world.validators.primary_port();
    let url = format!("http://127.0.0.1:{port}");
    let transaction = eth::send_call(
        &url,
        crate::internal::addresses::VOTE_ADDR,
        &world.validators.get(0).evm_key().unwrap(),
        &eth::IVote::createProposalWithVotingWindowCall {
            targetModule: outbe_primitives::addresses::CODE_REGISTRY_ADDRESS,
            payload: payload.clone(),
            votingWindowBlocks: 24,
        },
        None,
    )
    .expect("create real exact-revision Vote proposal");
    assert!(world.rpc.wait_successful_receipt(&transaction, 40));
    let proposal = world
        .rpc
        .proposal_id_from_receipt(port, &transaction)
        .unwrap();
    let mut ballots = Vec::new();
    for index in 0..4 {
        let validator = world.validators.get(index);
        let address = world.rpc.address_of(&validator.evm_key().unwrap()).unwrap();
        assert!(
            world.rpc.is_participant(port, &address).unwrap(),
            "only active validators vote"
        );
        let tx = world
            .rpc
            .cast_vote(&validator, proposal, true)
            .expect("real validator Vote");
        assert!(world.rpc.wait_successful_receipt(&tx, 40));
        ballots.push(tx);
    }
    let mut approval = None;
    wait_observation("exact Vote approval becomes finalized", || {
        let socket = world.localnet.radicle_control_socket(0);
        let record = eth::block_on(async move {
            LocalReceiptClient::connect(&socket)
                .await?
                .read_patch_approval(U256::from(proposal))
                .await
        })?
        .ok_or_else(|| eyre!("approval not finalized"))?;
        ensure!(
            record.approval.payload == payload,
            "approval changed the voted revision"
        );
        let blocks = world.rpc.proposal_approved_event_blocks(port, proposal)?;
        ensure!(
            blocks == [record.approval.approved_height],
            "approval event does not bind the same decision"
        );
        ensure!(
            world
                .rpc
                .finalized(port)
                .is_some_and(|h| h >= record.approval.approved_height),
            "approval not finalized on RPC"
        );
        approval = Some(serde_json::json!({
            "proposalId": proposal, "payload": payload,
            "approvedHeight": record.approval.approved_height,
            "approvedTimestamp": record.approval.approved_timestamp,
            "observedFinalizedBlock": record.block.number, "observedFinalizedHash": record.block.hash,
        }));
        Ok(())
    });
    let evidence = world.state.radicle.governments_patch.as_mut().unwrap();
    evidence.proposal_id = Some(proposal);
    evidence.proposal_transaction = Some(transaction);
    evidence.vote_transactions = ballots;
    evidence.approval = approval;
}

#[then("every validator verifies the finalized BLS Review and Merge for that revision")]
fn accepted(world: &mut World) {
    let patch = patch_evidence(world);
    let mut observations = Vec::new();
    for index in 0..4 {
        let observation =
            wait_observation("native BLS Review and Merge installed on validator", || {
                verify_merged(world, index, &patch)
            });
        observations.push(observation);
    }
    let merges: Vec<_> = observations
        .iter()
        .map(|o| o["sigrefs"].as_str().unwrap())
        .collect();
    assert!(merges.iter().all(|oid| *oid == merges[0]));
    world
        .state
        .radicle
        .governments_patch
        .as_mut()
        .unwrap()
        .validators = observations;
}

#[when("a fresh validator joins after the governments merge")]
fn join(world: &mut World) {
    world
        .localnet
        .assert_fresh_governments_validator(repository_id(world))
        .unwrap();
    promote(world);
    // The existing real FullNode -> validator transition runs DKG and activates membership.
    activate_joiner(world);
}

#[then("the fresh validator verifies the accepted governments history without voting again")]
fn joined(world: &mut World) {
    let patch = patch_evidence(world);
    let index = world.validators.joiner_index();
    let observation = wait_observation(
        "fresh validator independently reconstructs accepted history",
        || verify_merged(world, index, &patch),
    );
    let proposal = patch.proposal_id.unwrap();
    let voters = world
        .rpc
        .get_proposal_voters_on(
            world.validators.http_port(index),
            proposal,
            U256::ZERO,
            U256::from(64),
        )
        .expect("new validator reads old voters");
    let joiner_address = world
        .rpc
        .address_of(&world.validators.joiner().evm_key().unwrap())
        .unwrap()
        .parse::<alloy_primitives::Address>()
        .unwrap();
    assert!(!voters.contains(&joiner_address));
    let expected = world
        .state
        .radicle
        .founder_validator_addresses
        .iter()
        .map(|a| a.parse::<alloy_primitives::Address>().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(voters.into_iter().collect::<BTreeSet<_>>(), expected);
    assert_eq!(
        world
            .state
            .radicle
            .governments_patch
            .as_ref()
            .unwrap()
            .vote_transactions
            .len(),
        4
    );
    world
        .state
        .radicle
        .governments_patch
        .as_mut()
        .unwrap()
        .fresh_validator = Some(observation);
}

#[then("a fresh ordinary Radicle reader fetches the merged governments history")]
fn ordinary_reader(world: &mut World) {
    let rid = repository_id(world);
    let home = world
        .localnet
        .fetch_governments_as_user(rid, "governments-reader")
        .unwrap();
    assert!(
        !home.join("storage/.publication").exists(),
        "ordinary reader must not install an Outbe policy"
    );
    let repo = Repository::open(home.join("storage").join(rid.canonical()), rid).unwrap();
    let patch = patch_evidence(world);
    let observation = assert_merged_native(&repo, &patch).unwrap();
    world
        .state
        .radicle
        .governments_patch
        .as_mut()
        .unwrap()
        .ordinary_reader = Some(observation);
}

fn repository_id(world: &World) -> RepoId {
    world
        .state
        .radicle
        .repo_id
        .as_ref()
        .unwrap()
        .parse()
        .unwrap()
}
fn patch_evidence(world: &World) -> GovernmentsPatchEvidenceV1 {
    world
        .state
        .radicle
        .governments_patch
        .as_ref()
        .unwrap()
        .clone()
}

fn assert_patch_revision(
    proposed: &patch::Patch,
    expected: &GovernmentsPatchEvidenceV1,
) -> Result<()> {
    let revision = proposed
        .revision(&expected.revision_id.parse::<git::Oid>()?.into())
        .ok_or_else(|| eyre!("voted revision missing"))?;
    let author: RepositoryKey = expected
        .author
        .parse::<radicle::crypto::PublicKey>()?
        .into();
    ensure!(
        revision.author().public_key() == &author && author.as_ed25519().is_some(),
        "revision was not signed by the independent Ed25519 author"
    );
    ensure!(
        revision.head().to_string() == expected.head
            && revision.base().to_string() == expected.base,
        "patch revision changed"
    );
    ensure!(
        proposed.latest().0.to_string() == expected.revision_id,
        "patch revision replaced"
    );
    Ok(())
}

fn assert_merged_native(
    repo: &Repository,
    expected: &GovernmentsPatchEvidenceV1,
) -> Result<serde_json::Value> {
    let doc = repo.identity_doc()?;
    ensure!(
        doc.doc.delegates().len() == 1 && doc.doc.threshold() == 1,
        "network must be the only owner"
    );
    let owner = *doc.doc.delegates().first().as_key();
    ensure!(owner.as_ed25519().is_none(), "network owner must be BLS");
    let signed = radicle::storage::refs::SignedRefs::load(owner, repo)?;
    ensure!(signed.is_some(), "native BLS signed refs missing");
    ensure!(
        repo.head()?.1.to_string() == expected.head,
        "canonical main is not the approved revision head"
    );
    let store = patch::Patches::open(repo, ReadOnly)?;
    let proposed = store
        .get(&expected.patch_id.parse()?)?
        .ok_or_else(|| eyre!("patch missing"))?;
    assert_patch_revision(&proposed, expected)?;
    let revision = proposed
        .revision(&expected.revision_id.parse::<git::Oid>()?.into())
        .unwrap();
    ensure!(
        revision.review_by(&owner).and_then(|r| r.verdict()) == Some(patch::Verdict::Accept),
        "network BLS Review Accept missing"
    );
    ensure!(proposed.is_merged(), "patch not merged");
    ensure!(
        proposed.merges().any(|(actor, merge)| *actor == owner
            && merge.revision.to_string() == expected.revision_id
            && merge.commit.to_string() == expected.head),
        "network Merge does not bind voted revision"
    );
    let actual = repo.references_of(&owner)?;
    Ok(serde_json::json!({
        "repository": repo.id().to_string(), "owner": owner.to_string(),
        "patch": expected.patch_id, "revision": expected.revision_id,
        "head": repo.head()?.1.to_string(),
        "sigrefs": actual.get(&radicle::storage::refs::SIGREFS_BRANCH).unwrap().to_string(),
    }))
}

fn verify_merged(
    world: &World,
    index: usize,
    expected: &GovernmentsPatchEvidenceV1,
) -> Result<serde_json::Value> {
    let (repo, snapshot, history) = verified_history(world, index, expected.proposal_id)?;
    let mut desired = snapshot.owner_refs().clone();
    desired.insert(
        radicle::storage::refs::SIGREFS_BRANCH.to_ref_string(),
        snapshot.sigrefs(),
    );
    let owner: RepositoryKey =
        BlsPublicKey::from_bytes(snapshot.checkpoint().receipt().body.owner_bls_public.0)?.into();
    ensure!(
        repo.references_of(&owner)? == desired,
        "owner refs not at accepted checkpoint"
    );
    let mut observed = assert_merged_native(&repo, expected)?;
    observed["validator"] = index.into();
    observed["acceptedHistory"] = history.into();
    Ok(observed)
}

/// Verify own finalized receipts against reconstructed native proofs; never install refs or sign.
fn verified_history(
    world: &World,
    index: usize,
    proposal: Option<u64>,
) -> Result<(Repository, CheckpointOwnerSnapshot, Vec<serde_json::Value>)> {
    let socket = world.localnet.radicle_control_socket(index);
    let client = eth::block_on(async move { LocalReceiptClient::connect(&socket).await })?;
    let local = client.clone();
    let root = eth::block_on(async move { local.read(1).await })?
        .ok_or_else(|| eyre!("bootstrap receipt unavailable"))?;
    let rid = repository_id(world);
    ensure!(
        root.receipt().body.repo_id.0 == rid.into_sha1().unwrap(),
        "different network repository"
    );
    let home = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{index}/radicle"));
    let key = radicle::crypto::ssh::Keystore::new(&home.join("keys"))
        .public_key()?
        .ok_or_else(|| eyre!("validator identity missing"))?;
    let storage = radicle::Storage::open(
        home.join("storage"),
        git::UserInfo {
            alias: radicle::node::Alias::new("E2E verifier"),
            key,
        },
    )?;
    let policy = Arc::new(AcceptedRepositoryPolicy::pending_receipt(&root)?);
    let owner = BlsPublicKey::from_bytes(root.receipt().body.owner_bls_public.0)?;
    let owner: RepositoryKey = owner.into();
    let managed: serde_json::Value = serde_json::from_slice(&fs::read(
        home.join("storage/.publication")
            .join(format!("{}.json", rid.canonical())),
    )?)?;
    ensure!(
        managed
            == serde_json::json!({
                "version": 1, "repository": rid, "owner": owner,
                "policy": radicle::storage::publication::PublicationPolicy::id(policy.as_ref()),
            }),
        "runtime managed owner pin missing or incorrect"
    );
    storage.manage_repository(rid, owner, policy.clone())?;
    let repo = storage.repository(rid)?;
    let manifest = read_manifest(&home, &root)?;
    let verifier = AcceptedPublicationVerifier::from_receipt(root.clone(), manifest.clone())?;
    let genesis: serde_json::Value = serde_json::from_slice(&fs::read(
        world.localnet.scenario_dir().join("genesis.json"),
    )?)?;
    let descriptor = GovernmentsDescriptor::from_bytes(&hex::decode(
        genesis["config"]["radicleGovernments"]
            .as_str()
            .ok_or_else(|| eyre!("genesis descriptor missing"))?
            .trim_start_matches("0x"),
    )?)?;
    policy.accept_bootstrap(&verify_accepted_bootstrap(&repo, &descriptor, &verifier)?)?;
    let mut snapshot = CheckpointOwnerSnapshot::bootstrap(root.clone(), &manifest)?;
    let mut history = vec![receipt_evidence(world, index, &root, &manifest)?];
    let mut reviews = BTreeMap::new();
    let mut sequence = 2;
    let mut merged = false;
    while let Some(receipt) = eth::block_on({
        let local = client.clone();
        async move { local.read(sequence).await }
    })? {
        let body = &receipt.receipt().body;
        let manifest = read_manifest(&home, &receipt)?;
        let verifier =
            AcceptedPublicationVerifier::from_receipt(receipt.clone(), manifest.clone())?;
        match body.action {
            PublicationAction::Review | PublicationAction::Merge => {
                let local = client.clone();
                let proposal_id = body.proposal_id;
                let approval =
                    eth::block_on(async move { local.read_patch_approval(proposal_id).await })?
                        .ok_or_else(|| eyre!("own finalized approval missing"))?;
                if body.action == PublicationAction::Review {
                    let proof =
                        verify_accepted_review_publication(&repo, &approval, &snapshot, &verifier)?;
                    policy.accept_review(&proof)?;
                    reviews.insert(body.proposal_id, proof);
                } else {
                    let proof = verify_accepted_merge_publication(
                        &repo,
                        &approval,
                        &snapshot,
                        reviews
                            .get(&body.proposal_id)
                            .ok_or_else(|| eyre!("accepted Review missing"))?,
                        &verifier,
                    )?;
                    policy.accept_merge(&proof)?;
                    merged = proposal.is_some_and(|id| body.proposal_id == U256::from(id));
                }
            }
            PublicationAction::BindMaterial => policy
                .accept_material(&verify_accepted_material_publication(&snapshot, &verifier)?)?,
            _ => return Err(eyre!("unexpected bootstrap inside accepted history")),
        }
        snapshot.advance(receipt.clone(), &manifest)?;
        history.push(receipt_evidence(world, index, &receipt, &manifest)?);
        if merged {
            break;
        }
        sequence += 1;
    }
    ensure!(
        proposal.is_none() || merged,
        "Merge checkpoint not finalized yet"
    );
    Ok((repo, snapshot, history))
}

fn read_manifest(
    home: &Path,
    receipt: &FinalizedPublicationReceipt,
) -> Result<PublicationManifest> {
    let path = home
        .join("node/outbe-governments/packages/packages")
        .join(hex::encode(receipt.receipt().receipt_hash))
        .join("manifest");
    let bytes = fs::read(path)?;
    ensure!(
        bytes.len() <= outbe_radicle::integration::publication::MAX_MANIFEST_BYTES,
        "oversized accepted manifest"
    );
    let manifest = PublicationManifest::from_bytes(&bytes)?;
    Ok(manifest)
}

fn receipt_evidence(
    world: &World,
    index: usize,
    receipt: &FinalizedPublicationReceipt,
    manifest: &PublicationManifest,
) -> Result<serde_json::Value> {
    let port = world.validators.http_port(index);
    ensure!(
        world
            .rpc
            .block_hash(port, receipt.block().number)
            .ok_or_else(|| eyre!("canonical finalized block unavailable"))?
            == format!("{:#x}", receipt.block().hash),
        "receipt block is not canonical"
    );
    ensure!(
        world
            .rpc
            .finalized(port)
            .is_some_and(|h| h >= receipt.receipt().accepted_height),
        "publication not finalized"
    );
    Ok(serde_json::json!({
        "action": format!("{:?}", receipt.receipt().body.action),
        "receiptHash": receipt.receipt().receipt_hash,
        "acceptedHeight": receipt.receipt().accepted_height,
        "finalizedBlockNumber": receipt.block().number, "finalizedBlockHash": receipt.block().hash,
        "bodyHex": hex::encode(receipt.receipt().body.to_bytes()),
        "manifestHex": hex::encode(manifest.to_bytes()?),
    }))
}

fn wait_observation<T>(label: &str, mut observe: impl FnMut() -> Result<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        match observe() {
            Ok(value) => return value,
            Err(error) if std::time::Instant::now() >= deadline => panic!("{label}: {error:#}"),
            Err(_) => sleep(Duration::from_millis(250)),
        }
    }
}
