use super::*;
use commonware_codec::Encode as _;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_utils::{Participant, TryCollect as _};
use outbe_consensus::{bls::bootstrap_dkg_for_participants, hybrid::VrfMaterialProvider};
use radicle::identity::doc::GetPayload as _;
use radicle::{crypto::Signer as _, storage::WriteStorage as _};
mod local_review;

#[tokio::test]
async fn independent_collectors_prepare_identical_bootstrap_without_promoting_refs() {
    let mut keys: Vec<_> = (101..105).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key());
    let dkg = bootstrap_dkg_for_participants(
        keys.iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap(),
    )
    .unwrap();
    let owner = BlsPublicKey::from_bytes(
        dkg.polynomial
            .public()
            .encode()
            .as_ref()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    let authority = crate::manager::FinalizedRepositoryAuthority {
        block: crate::manager::FinalizedBlock {
            number: 12,
            hash: B256::repeat_byte(12),
        },
        chain_id: 2026,
        genesis_hash: B256::repeat_byte(7),
        epoch: 2,
        committee: outbe_validatorset::state::CommitteeSnapshot {
            committee: keys
                .iter()
                .enumerate()
                .map(|(i, key)| outbe_validatorset::state::CommitteeEntry {
                    address: alloy_primitives::Address::repeat_byte(i as u8 + 1),
                    consensus_pubkey: key.public_key().encode().as_ref().try_into().unwrap(),
                })
                .collect(),
            vrf_material_version: 7,
            vrf_group_public_key_bytes: dkg.polynomial.public().encode().to_vec(),
            vrf_public_polynomial_hash: outbe_consensus::dkg_manager::public_polynomial_hash(
                &dkg.polynomial,
            ),
        },
    };
    let material =
        RepositorySigningMaterial::new(authority.clone(), dkg.output.clone(), owner).unwrap();
    let providers: Vec<_> = dkg
        .shares
        .iter()
        .map(|share| VrfMaterialProvider::new(7, dkg.polynomial.clone(), Some(share.clone())))
        .collect();
    let sign = |subject: &RepositorySubject, first: usize| {
        let partials: Vec<_> = (first..first + 3)
            .map(|i| {
                providers[i]
                    .sign_repository(7, Participant::from_usize(i), &dkg.polynomial, subject)
                    .unwrap()
            })
            .collect();
        let recovered = providers[first]
            .recover_repository(7, &dkg.polynomial, subject, &partials)
            .unwrap();
        BlsEnvelope::attach(
            subject.clone(),
            owner,
            recovered.encode().as_ref().try_into().unwrap(),
        )
        .unwrap()
    };
    let frames = |subject: &RepositorySubject, first: usize| {
        (first..first + 3)
            .map(|i| {
                let partial = providers[i]
                    .sign_repository(7, Participant::from_usize(i), &dkg.polynomial, subject)
                    .unwrap();
                let body = [
                    Sha256::hash(&[subject.as_bytes()]).0.as_slice(),
                    &authority.chain_id.to_be_bytes(),
                    authority.genesis_hash.as_slice(),
                    &authority.epoch.to_be_bytes(),
                    &7u64.to_be_bytes(),
                    &material.material_hash(),
                    &(i as u32).to_be_bytes(),
                    partial.value.encode().as_ref(),
                ]
                .concat();
                let signature = keys[i].sign(b"outbe.radicle.partial.v1", &body);
                super::super::signing::RepositoryPartial::from_bytes(
                    &[body, signature.encode().to_vec()].concat(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>()
    };
    let descriptor = GovernmentsDescriptor {
        chain_id: 2026,
        genesis_timestamp: 1700000000,
        description: "я".repeat(128),
        initial_tree_oid: "0x4b825dc642cb6eb9a060e54bf8d69288fbee4904"
            .parse()
            .unwrap(),
        initial_content_manifest_sha256: Sha256::hash(&[b"[]"]).0.into(),
    };
    let contributions = |frames: &[super::super::signing::RepositoryPartial]| {
        frames
            .iter()
            .map(|frame| {
                (
                    keys[frame.participant_index() as usize].public_key(),
                    frame.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let state = crate::manager::FinalizedBootstrapSigningState {
        authority: authority.clone(),
        descriptor: descriptor.clone(),
    };
    use super::super::network::{material_tests::bootstrap_partials, RepositoryBootstrapJob};
    use std::sync::Arc;
    let mut results = Vec::new();
    for first in [0, 1] {
        let temp = tempfile::tempdir().unwrap();
        let author = radicle::crypto::SigningKey::from_seed(radicle::crypto::Seed::new([41; 32]));
        let storage = radicle::Storage::open(
            temp.path().join("storage"),
            git::UserInfo {
                alias: radicle::node::Alias::new("Collector"),
                key: *author.public_key(),
            },
        )
        .unwrap();
        let document = prepare_bootstrap_document(&descriptor, &material).unwrap();
        let pending = Arc::new(
            super::super::publication::policy::AcceptedRepositoryPolicy::pending_bootstrap(
                &document,
            )
            .unwrap(),
        );
        storage
            .manage_repository(document.repository_id(), owner.into(), pending)
            .unwrap();
        let repo = storage.create(document.repository_id()).unwrap();
        let tree = repo.raw().treebuilder(None).unwrap().write().unwrap();
        assert_eq!(tree.as_bytes(), descriptor.initial_tree_oid.as_slice());
        let document = Arc::new(document);
        let actual = bootstrap_partials(
            RepositoryBootstrapJob::Document(document.clone()),
            &state,
            &dkg.output,
            &providers,
            &keys,
        )
        .await;
        let document_frames = actual[first..first + 3].to_vec();
        assert_eq!(
            document_frames
                .iter()
                .map(|p| p.to_bytes())
                .collect::<Vec<_>>(),
            frames(document.subject(), first)
                .iter()
                .map(|p| p.to_bytes())
                .collect::<Vec<_>>()
        );
        let document = Arc::try_unwrap(document).ok().unwrap();
        let document_contributions = contributions(&document_frames);
        assert!(material
            .recover_bootstrap_document(&document, &state, &document_contributions[..2])
            .is_err());
        let document_signature = material
            .recover_bootstrap_document(&document, &state, &document_contributions)
            .unwrap();
        assert_eq!(document_signature, sign(document.subject(), first));
        let mut wrong = state.clone();
        wrong.descriptor.description.push('!');
        assert!(material
            .sign_bootstrap_document(
                &providers[0],
                &document,
                &wrong,
                authority.committee.committee[0].address,
                &keys[0]
            )
            .is_err());
        assert!(material
            .sign_bootstrap_document(
                &providers[0],
                &document,
                &state,
                alloy_primitives::Address::repeat_byte(99),
                &keys[0]
            )
            .is_err());
        assert!(document_signature
            .verify(document.subject(), owner.into())
            .is_ok());
        assert!(prepare_bootstrap_document(&descriptor, &material)
            .unwrap()
            .attach(&repo, &document_signature, b"[]\n")
            .is_err());
        let identity = document.attach(&repo, &document_signature, b"[]").unwrap();
        assert!(prepare_bootstrap_document(&descriptor, &material)
            .unwrap()
            .attach(&repo, &sign(identity.subject(), first), b"[]")
            .is_err());
        let identity = Arc::new(identity);
        let actual = bootstrap_partials(
            RepositoryBootstrapJob::Identity(identity.clone()),
            &state,
            &dkg.output,
            &providers,
            &keys,
        )
        .await;
        let identity_frames = actual[first..first + 3].to_vec();
        let identity = Arc::try_unwrap(identity).ok().unwrap();
        let identity_signature = material
            .recover_bootstrap_identity(&identity, &state, &contributions(&identity_frames))
            .unwrap();
        assert_eq!(identity_signature, sign(identity.subject(), first));
        let refs = identity.attach(&repo, &identity_signature).unwrap();
        let refs = Arc::new(refs);
        let actual = bootstrap_partials(
            RepositoryBootstrapJob::Refs(refs.clone()),
            &state,
            &dkg.output,
            &providers,
            &keys,
        )
        .await;
        let refs_frames = actual[first..first + 3].to_vec();
        let refs = Arc::try_unwrap(refs).ok().unwrap();
        let refs_signature = material
            .recover_bootstrap_refs(&refs, &state, &contributions(&refs_frames))
            .unwrap();
        assert_eq!(refs_signature, sign(refs.subject(), first));
        let (body, manifest) = refs.attach(&repo, &refs_signature).unwrap();
        assert_eq!(repo.raw().references().unwrap().count(), 0);
        assert_eq!(body.descriptor_hash, descriptor.hash().unwrap());
        assert_eq!(body.finalized_anchor_height, 0);
        assert_eq!(body.finalized_anchor_hash, authority.genesis_hash());
        assert_eq!(manifest.subjects().len(), 3);
        let pending_identity = manifest.owner_refs().unwrap().get(&IDENTITY_ROOT).unwrap();
        assert!(repo.identity_doc_at(pending_identity).is_err());
        for object in manifest.objects() {
            let oid: git::Oid = object.oid.parse().unwrap();
            let odb = repo.raw().odb().unwrap();
            let data = odb.read(oid.into()).unwrap();
            assert_eq!(object.sha256, hex::encode(Sha256::hash(&[data.data()]).0));
        }
        let main = manifest
            .owner_refs()
            .unwrap()
            .head(git::fmt::refname!("main"))
            .unwrap();
        let commit = repo.raw().find_commit(main.into()).unwrap();
        assert_eq!(
            commit.tree_id().as_bytes(),
            descriptor.initial_tree_oid.as_slice()
        );
        assert_eq!(commit.parent_count(), 0);
        assert_eq!(commit.author().name().unwrap(), "Outbe Network");
        assert_eq!(commit.time().seconds(), 1700000000);
        let witness = super::super::signing::witness::RepositoryWitnessBundle::new(
            material.canonical_output(),
            vec![
                document_frames.clone(),
                identity_frames.clone(),
                refs_frames.clone(),
            ],
        )
        .unwrap();
        let short = super::super::signing::witness::RepositoryWitnessBundle::new(
            material.canonical_output(),
            vec![document_frames[..2].to_vec(), identity_frames, refs_frames],
        )
        .unwrap();
        let verified = Arc::new(
            verify_bootstrap_publication(
                &repo,
                BootstrapPublicationInput {
                    state: &state,
                    material: &material,
                    content_manifest: b"[]",
                },
                &body,
                &manifest,
                &witness,
            )
            .unwrap(),
        );
        let store_path = temp.path().join("publication-store");
        let mut store =
            super::super::publication::store::ReviewPackageStore::open(&store_path).unwrap();
        assert!(store
            .persist_and_select_bootstrap(&repo, verified.clone(), &short)
            .is_err());
        let selected = Arc::new(
            store
                .persist_and_select_bootstrap(&repo, verified, &witness)
                .unwrap(),
        );
        let attestations = super::super::network::material_tests::bootstrap_attestations(
            selected,
            &state,
            &dkg.output,
            &keys,
        )
        .await;
        assert_eq!(attestations.len(), 4);
        let accepted = admit_bootstrap(
            &descriptor,
            &authority.committee,
            &body,
            &attestations[first..first + 3],
        );
        let receipt = crate::manager::FinalizedPublicationReceipt {
            block: crate::manager::FinalizedBlock {
                number: 15,
                hash: B256::repeat_byte(15),
            },
            receipt: accepted,
        };
        let verifier = super::super::signing::accepted::AcceptedPublicationVerifier::from_receipt(
            receipt.clone(),
            manifest.clone(),
        )
        .unwrap();
        let slot = super::super::publication::store::review_slot_key(&body);
        drop(store);
        let store =
            super::super::publication::store::ReviewPackageStore::open(&store_path).unwrap();
        let (archived_descriptor, archived_content) =
            store.load_bootstrap_inputs(&receipt).unwrap();
        assert_eq!(archived_descriptor, descriptor);
        assert_eq!(archived_content, b"[]");
        let package_directory = store_path
            .join("packages")
            .join(hex::encode(body.receipt_hash()));
        let archived_descriptor_path = package_directory.join("descriptor");
        let mut wrong_descriptor = descriptor.clone();
        wrong_descriptor.description = "other descriptor".into();
        std::fs::write(
            &archived_descriptor_path,
            wrong_descriptor.to_bytes().unwrap(),
        )
        .unwrap();
        assert!(store.load_bootstrap_inputs(&receipt).is_err());
        std::fs::write(&archived_descriptor_path, descriptor.to_bytes().unwrap()).unwrap();
        let archived_content_path = package_directory.join("initial-content.manifest");
        std::fs::write(&archived_content_path, b"[]\n").unwrap();
        assert!(store.load_bootstrap_inputs(&receipt).is_err());
        std::fs::remove_file(&archived_content_path).unwrap();
        assert!(store.load_bootstrap_inputs(&receipt).is_err());
        std::fs::write(&archived_content_path, b"[]").unwrap();
        assert_eq!(store.load_bootstrap_inputs(&receipt).unwrap().1, b"[]");
        let replay_storage = radicle::Storage::open(
            temp.path().join("replay"),
            git::UserInfo {
                alias: radicle::node::Alias::new("Replay"),
                key: *author.public_key(),
            },
        )
        .unwrap();
        let replay_repo = replay_storage.create(repo.id()).unwrap();
        let loaded = store.load_selected(slot, &replay_repo).unwrap().unwrap();
        assert_eq!(loaded.body, body);
        assert_eq!(loaded.manifest, manifest);
        verify_bootstrap_publication(
            &replay_repo,
            BootstrapPublicationInput {
                state: &state,
                material: &material,
                content_manifest: b"[]",
            },
            &loaded.body,
            &loaded.manifest,
            &loaded.witness,
        )
        .unwrap();
        assert_eq!(replay_repo.raw().references().unwrap().count(), 0);
        assert!(verify_bootstrap_publication(
            &repo,
            BootstrapPublicationInput {
                state: &state,
                material: &material,
                content_manifest: b"[]"
            },
            &body,
            &manifest,
            &short,
        )
        .is_err());
        assert!(verify_bootstrap_publication(
            &repo,
            BootstrapPublicationInput {
                state: &state,
                material: &material,
                content_manifest: b"[]\n"
            },
            &body,
            &manifest,
            &witness,
        )
        .is_err());
        // An accepted history reader needs neither the old q transcript nor creation artifact.
        std::fs::remove_dir_all(package_directory.join("witnesses")).unwrap();
        std::fs::remove_dir_all(package_directory.join("ready")).unwrap();
        for slot in std::fs::read_dir(store_path.join("slots")).unwrap() {
            std::fs::remove_file(slot.unwrap().path()).unwrap();
        }
        std::fs::remove_file(&archived_content_path).unwrap();
        assert!(store.load_selected(slot, &replay_repo).unwrap().is_none());
        let accepted = store
            .load_accepted(&receipt, &replay_repo)
            .unwrap()
            .unwrap();
        assert_eq!(accepted.body, body);
        assert_eq!(accepted.manifest, manifest);
        assert_eq!(
            store.load_accepted_descriptor(&receipt).unwrap(),
            descriptor
        );
        let proof = verify_accepted_bootstrap(&repo, &descriptor, &verifier).unwrap();
        let snapshot =
            super::super::publication::CheckpointOwnerSnapshot::bootstrap(receipt, &manifest)
                .unwrap();
        let policy = std::sync::Arc::new(
            super::super::publication::policy::AcceptedRepositoryPolicy::new(&snapshot).unwrap(),
        );
        use radicle::storage::publication::{EntryRequest, PublicationPolicy as _};
        let identity = snapshot.owner_refs().get(&IDENTITY_ROOT).unwrap();
        let request = EntryRequest {
            repository: repo.id(),
            authority: owner.into(),
            entry: identity,
            kind: radicle::crypto::SubjectKind::Identity,
        };
        assert!(policy.authorize_entry(&request).is_err());
        policy.accept_bootstrap(&proof).unwrap();
        policy.accept_bootstrap(&proof).unwrap();
        assert_eq!(
            policy.authorize_entry(&request).unwrap().context(),
            native_policy_context(&body, 0)
        );
        let mut forged = request;
        forged.entry = main;
        assert!(policy.authorize_entry(&forged).is_err());
        storage
            .manage_repository(repo.id(), owner.into(), policy)
            .unwrap();
        assert_eq!(
            repo.identity_doc_at(identity)
                .unwrap()
                .doc
                .project()
                .unwrap()
                .unwrap()
                .description(),
            descriptor.description
        );
        assert_eq!(repo.raw().references().unwrap().count(), 0);
        let main_ref = git::fmt::qualified!("refs/heads/main")
            .with_namespace((&RepositoryKey::from(owner)).into())
            .to_string();
        repo.raw()
            .reference(&main_ref, identity.into(), false, "conflicting target")
            .unwrap();
        assert!(matches!(proof.install(&repo), Err(Error::RefConflict)));
        assert_eq!(repo.raw().references().unwrap().count(), 1);
        assert_eq!(
            repo.raw().refname_to_id(&main_ref).unwrap(),
            identity.into()
        );
        repo.raw()
            .find_reference(&main_ref)
            .unwrap()
            .delete()
            .unwrap();
        radicle::cob::object::Storage::update(
            &repo,
            &RepositoryKey::from(owner),
            &identity::TYPENAME,
            &identity.into(),
            &identity,
        )
        .unwrap();
        proof.install(&repo).unwrap();
        proof.install(&repo).unwrap();
        assert_eq!(
            repo.raw()
                .refname_to_id(git::refs::storage::sigrefs(&owner.into()).as_str())
                .unwrap(),
            snapshot.sigrefs().into()
        );
        assert_eq!(
            radicle::cob::identity::Identity::get(&identity.into(), &repo)
                .unwrap()
                .doc()
                .encode()
                .unwrap()
                .0,
            git::Oid::from_sha1(repo.id().into_sha1().unwrap())
        );
        if first == 0 {
            local_review::assert_review_socket(
                &repo,
                (store, store_path.as_path()),
                proof.receipt().clone(),
                &material,
                &keys,
                &providers,
                &dkg.output,
            )
            .await;
        }
        results.push((body, manifest.to_bytes().unwrap()));
    }
    assert_eq!(results[0], results[1]);
    let mut other_network = authority;
    other_network.genesis_hash = B256::repeat_byte(8);
    let material = RepositorySigningMaterial::new(other_network, dkg.output, owner).unwrap();
    let other = prepare_bootstrap_document(&descriptor, &material).unwrap();
    assert_eq!(other.body().repo_id, results[0].0.repo_id);
    assert_ne!(
        native_policy_context(other.body(), 0),
        native_policy_context(&results[0].0, 0)
    );
    let mut invalid = descriptor.clone();
    invalid.chain_id += 1;
    assert!(prepare_bootstrap_document(&invalid, &material).is_err());
    invalid = descriptor.clone();
    invalid.description.push('x');
    assert!(prepare_bootstrap_document(&invalid, &material).is_err());
    invalid = descriptor;
    invalid.genesis_timestamp = u64::MAX;
    assert!(prepare_bootstrap_document(&invalid, &material).is_err());
}

fn admit_bootstrap(
    descriptor: &GovernmentsDescriptor,
    committee: &outbe_validatorset::state::CommitteeSnapshot,
    body: &PublicationBody,
    attestations: &[outbe_primitives::radicle_publication::PublicationAttestation],
) -> outbe_coderegistry::publication::PublicationReceipt {
    use outbe_coderegistry::CodeRegistry;
    use outbe_primitives::{
        radicle_publication::PublicationBatch, storage::hashmap::HashMapStorageProvider,
    };
    use outbe_validatorset::{contract::ValidatorSet, state::write_committee_snapshot};
    let mut state =
        HashMapStorageProvider::new_with_chain_identity(body.chain_id, body.genesis_hash);
    state.set_block_number(1);
    state.enter(|storage| {
        let mut validators = ValidatorSet::new(storage.clone());
        validators
            .config_owner
            .write(alloy_primitives::Address::repeat_byte(99))
            .unwrap();
        validators.set_config_max_validators(4).unwrap();
        for member in &committee.committee {
            validators
                .test_register_validator_without_pop(member.address, &member.consensus_pubkey)
                .unwrap();
            validators
                .activate_validator_via_boundary_for_test(member.address)
                .unwrap();
        }
        validators.advance_epoch(1001, 1).unwrap();
        validators.advance_epoch(1002, 2).unwrap();
        write_committee_snapshot(storage.clone(), 2, committee).unwrap();
        let registry = CodeRegistry::new(storage);
        registry.config_max_repositories.write(8).unwrap();
        registry
            .config_descriptor_hash
            .write(descriptor.hash().unwrap())
            .unwrap();
        registry
            .config_descriptor_bytes
            .write(&descriptor.to_bytes().unwrap())
            .unwrap();
    });
    state.set_block_number(14);
    let pending = PublicationBatch {
        body: body.clone(),
        attestations: attestations[..2].to_vec(),
    };
    let progress = state
        .enter(|s| CodeRegistry::new(s).apply_publication_batch(&pending))
        .unwrap();
    assert_eq!(progress.attestations, 2);
    assert!(progress.receipt.is_none());
    state.enter(|s| {
        assert!(CodeRegistry::new(s)
            .network_repository
            .read()
            .unwrap()
            .is_zero())
    });
    let completing = PublicationBatch {
        body: body.clone(),
        attestations: attestations[2..].to_vec(),
    };
    let accepted = state
        .enter(|s| CodeRegistry::new(s).apply_publication_batch(&completing))
        .unwrap()
        .receipt
        .unwrap();
    assert_eq!(accepted.body, *body);
    assert_eq!(accepted.receipt_hash, body.receipt_hash());
    assert_eq!(accepted.accepted_height, 14);
    state.enter(|s| {
        let registry = CodeRegistry::new(s);
        assert_eq!(
            outbe_coderegistry::repo_id_from_storage_key(
                registry.network_repository.read().unwrap()
            )
            .unwrap(),
            body.repo_id
        );
        assert_eq!(registry.publication_sequence.read().unwrap(), 1);
        assert_eq!(registry.publication_receipt(1).unwrap().unwrap(), accepted);
    });
    accepted
}

#[test]
fn accepted_native_history_recovers_without_local_packages_or_signing_material() {
    use crate::integration::publication::{
        history::AcceptedHistory, policy::AcceptedRepositoryPolicy, store::ReviewPackageStore,
    };
    use commonware_cryptography::bls12381::primitives::{
        group::{Private, Scalar},
        ops,
        variant::MinSig,
    };
    use radicle::crypto::SubjectKind;
    use radicle::storage::publication::{EntryRequest, PublicationPolicy as _};
    use radicle::storage::ReadStorage as _;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    // Only this unit fixture owns a whole secret; recovery itself receives public native objects.
    let secret = Private::new(Scalar::from_u64(42));
    let owner = BlsPublicKey::from_bytes(
        ops::compute_public::<MinSig>(&secret)
            .encode()
            .as_ref()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    let sign = |subject: &RepositorySubject| {
        let signature = ops::sign_message::<MinSig>(
            &secret,
            radicle::crypto::repository::DOMAIN,
            subject.as_bytes(),
        );
        BlsEnvelope::attach(
            subject.clone(),
            owner,
            signature.encode().as_ref().try_into().unwrap(),
        )
        .unwrap()
    };
    let temp = tempfile::tempdir().unwrap();
    let author = radicle::crypto::SigningKey::from_seed(radicle::crypto::Seed::new([41; 32]));
    let storage = radicle::Storage::open(
        temp.path().join("storage"),
        git::UserInfo {
            alias: radicle::node::Alias::new("Recovery"),
            key: *author.public_key(),
        },
    )
    .unwrap();
    let mut descriptor = GovernmentsDescriptor {
        chain_id: 2026,
        genesis_timestamp: 1700000000,
        description: "Network policy".into(),
        initial_tree_oid: Default::default(),
        initial_content_manifest_sha256: Sha256::hash(&[b"[]"]).0.into(),
    };
    let (blob, _) = encode_document(&descriptor, owner).unwrap();
    let repo = storage.create(blob.into()).unwrap();
    let mut body = PublicationBody {
        chain_id: 2026,
        genesis_hash: B256::repeat_byte(7),
        repo_id: blob.into_sha1().unwrap().into(),
        descriptor_hash: B256::ZERO,
        owner_bls_public: (*owner.as_bytes()).into(),
        players_commitment: B256::repeat_byte(9),
        epoch: 2,
        material_version: 7,
        material_hash: B256::repeat_byte(10),
        finalized_anchor_height: 0,
        finalized_anchor_hash: B256::repeat_byte(7),
        sequence: 1,
        previous_receipt_hash: B256::ZERO,
        action: PublicationAction::Bootstrap,
        proposal_id: Default::default(),
        patch_id: Default::default(),
        revision_id: Default::default(),
        expected_target_head: Default::default(),
        package_manifest_hash: B256::ZERO,
    };
    let document_signature = sign(&RepositorySubject::identity(
        body.repo_id.0,
        native_policy_context(&body, 0),
        body.repo_id.0,
    ));
    // A copied valid envelope in ordinary source content must never participate in signature discovery.
    let decoy = repo.raw().blob(&document_signature.to_bytes()).unwrap();
    let mut tree = repo.raw().treebuilder(None).unwrap();
    tree.insert("source.bin", decoy, 0o100644).unwrap();
    descriptor.initial_tree_oid =
        alloy_primitives::FixedBytes::from_slice(tree.write().unwrap().as_bytes());
    body.descriptor_hash = descriptor.hash().unwrap();
    let document = accepted_document(&descriptor, &body).unwrap();
    let identity = document.attach(&repo, &document_signature, b"[]").unwrap();
    let identity_signature = sign(identity.subject());
    let refs = identity.attach(&repo, &identity_signature).unwrap();
    let refs_signature = sign(refs.subject());
    let (body, expected) = refs.attach(&repo, &refs_signature).unwrap();
    let checkpoint = crate::manager::FinalizedPublicationReceipt {
        block: crate::manager::FinalizedBlock {
            number: 20,
            hash: B256::repeat_byte(20),
        },
        receipt: outbe_coderegistry::publication::PublicationReceipt {
            accepted_height: 18,
            receipt_hash: body.receipt_hash(),
            body,
        },
    };
    let policy = Arc::new(AcceptedRepositoryPolicy::pending_receipt(&checkpoint).unwrap());
    storage
        .manage_repository(repo.id(), owner.into(), policy.clone())
        .unwrap();
    let entry = expected.owner_refs().unwrap().get(&IDENTITY_ROOT).unwrap();
    let request = EntryRequest {
        repository: repo.id(),
        authority: owner.into(),
        entry,
        kind: SubjectKind::Cob,
    };
    assert!(policy.authorize_entry(&request).is_err());
    let owner_key: RepositoryKey = owner.into();
    let advertised = vec![(
        git::refs::storage::sigrefs(&owner_key).into(),
        expected
            .refs()
            .iter()
            .find(|r| r.name == SIGREFS_BRANCH.as_str())
            .unwrap()
            .new
            .parse()
            .unwrap(),
    )];
    policy
        .prepare_history(AcceptedHistory {
            descriptor: descriptor.clone(),
            receipts: vec![checkpoint.clone()],
            approvals: BTreeMap::new(),
        })
        .unwrap();
    let mut wrong_tip = advertised.clone();
    wrong_tip[0].1 = entry;
    assert!(policy.prepare_fetch(&repo, &wrong_tip).is_err());
    assert!(policy.authorize_entry(&request).is_err());
    policy.prepare_fetch(&repo, &advertised).unwrap();
    assert_eq!(policy.recovered_manifests().unwrap()[0].1, expected);
    assert!(policy.authorize_entry(&request).is_ok());
    assert!(repo.raw().references().unwrap().next().is_none());
    let mut store = ReviewPackageStore::open(temp.path().join("packages")).unwrap();
    store
        .persist_accepted_descriptor(&checkpoint, &descriptor)
        .unwrap();
    assert!(store.load_accepted(&checkpoint, &repo).unwrap().is_none());
    store
        .persist_checkpoint_manifest(&checkpoint, &expected)
        .unwrap();
    assert_eq!(
        store
            .load_accepted(&checkpoint, &repo)
            .unwrap()
            .unwrap()
            .manifest,
        expected
    );
    assert_eq!(
        store.load_accepted_descriptor(&checkpoint).unwrap(),
        descriptor
    );

    struct LocalHistory(
        crate::manager::FinalizedPublicationReceipt,
        GovernmentsDescriptor,
    );
    impl crate::manager::RepositoryReceiptReader for LocalHistory {
        fn read_governments_descriptor(
            &self,
        ) -> Result<Option<GovernmentsDescriptor>, crate::manager::ManagerError> {
            Ok(Some(self.1.clone()))
        }
        fn read_current_publication_receipt(
            &self,
            sequence: u64,
        ) -> Result<Option<crate::manager::FinalizedPublicationReceipt>, crate::manager::ManagerError>
        {
            Ok((sequence == 1).then(|| self.0.clone()))
        }
    }
    // A manifest-only SDK cache repairs its public descriptor from the same local chain reader.
    std::fs::remove_file(
        temp.path()
            .join("packages/packages")
            .join(hex::encode(checkpoint.receipt().receipt_hash))
            .join("descriptor"),
    )
    .unwrap();
    let reopened = store
        .open_checkpoint_repository(
            &LocalHistory(checkpoint.clone(), descriptor.clone()),
            &checkpoint,
            repo.raw().path(),
            git::UserInfo {
                alias: radicle::node::Alias::new("Recovery"),
                key: *author.public_key(),
            },
        )
        .unwrap();
    assert!(reopened.identity_doc_at(entry).is_ok());
    assert_eq!(
        store.load_accepted_descriptor(&checkpoint).unwrap(),
        descriptor
    );
    let empty_storage = radicle::Storage::open(
        temp.path().join("empty"),
        git::UserInfo {
            alias: radicle::node::Alias::new("Empty"),
            key: *author.public_key(),
        },
    )
    .unwrap();
    let empty = empty_storage.create(repo.id()).unwrap();
    assert!(store.load_accepted(&checkpoint, &empty).unwrap().is_none());

    // A present tip with a missing reachable blob requests one complete native fetch.
    let tip = repo.raw().find_commit(advertised[0].1.into()).unwrap();
    let signature_oid = tip
        .tree()
        .unwrap()
        .get_name(radicle::storage::refs::SIGNATURE_BLOB_PATH)
        .unwrap()
        .id();
    let signature_bytes = repo
        .raw()
        .find_blob(signature_oid)
        .unwrap()
        .content()
        .to_vec();
    let hash = signature_oid.to_string();
    std::fs::remove_file(
        repo.raw()
            .path()
            .join("objects")
            .join(&hash[..2])
            .join(&hash[2..]),
    )
    .unwrap();
    let incomplete = storage.repository(repo.id()).unwrap();
    assert!(incomplete
        .raw()
        .odb()
        .unwrap()
        .exists(advertised[0].1.into()));
    assert!(store
        .load_accepted(&checkpoint, &incomplete)
        .unwrap()
        .is_none());
    let repair = AcceptedRepositoryPolicy::pending_receipt(&checkpoint).unwrap();
    repair
        .prepare_history(AcceptedHistory {
            descriptor: descriptor.clone(),
            receipts: vec![checkpoint.clone()],
            approvals: BTreeMap::new(),
        })
        .unwrap();
    assert!(matches!(
        repair.prepare_fetch(&incomplete, &advertised),
        Err(radicle::storage::publication::Error::MissingObjects)
    ));
    assert!(repair.authorize_entry(&request).is_err());
    assert_eq!(
        incomplete.raw().blob(&signature_bytes).unwrap(),
        signature_oid
    );
    repair.prepare_fetch(&incomplete, &advertised).unwrap();
    assert!(repair.authorize_entry(&request).is_ok());

    let mut bad = checkpoint.clone();
    bad.receipt.body.package_manifest_hash = B256::repeat_byte(42);
    bad.receipt.receipt_hash = bad.receipt.body.receipt_hash();
    let denied = AcceptedRepositoryPolicy::pending_receipt(&bad).unwrap();
    denied
        .prepare_history(AcceptedHistory {
            descriptor: descriptor.clone(),
            receipts: vec![bad],
            approvals: BTreeMap::new(),
        })
        .unwrap();
    assert!(denied.prepare_fetch(&repo, &advertised).is_err());
    assert!(denied.authorize_entry(&request).is_err());

    let mut next = checkpoint.clone();
    next.receipt.body.sequence = 2;
    next.receipt.body.previous_receipt_hash = checkpoint.receipt().receipt_hash;
    next.receipt.body.action = PublicationAction::BindMaterial;
    next.receipt.body.material_version += 1;
    let manifest =
        PublicationManifest::new(vec![], vec![], vec![], expected.owner_snapshot().to_vec())
            .unwrap();
    next.receipt.body.package_manifest_hash = manifest.hash().unwrap();
    next.receipt.receipt_hash = next.receipt.body.receipt_hash();
    assert!(policy
        .prepare_history(AcceptedHistory {
            descriptor: descriptor.clone(),
            receipts: vec![checkpoint.clone(), next.clone()],
            approvals: BTreeMap::new()
        })
        .unwrap());
    policy.prepare_fetch(&repo, &advertised).unwrap();
    assert_eq!(policy.recovered_manifests().unwrap().len(), 2);
    let mut conflict = next;
    conflict.receipt.body.material_version += 1;
    conflict.receipt.receipt_hash = conflict.receipt.body.receipt_hash();
    assert!(policy
        .prepare_history(AcceptedHistory {
            descriptor,
            receipts: vec![checkpoint, conflict],
            approvals: BTreeMap::new()
        })
        .is_err());
}
