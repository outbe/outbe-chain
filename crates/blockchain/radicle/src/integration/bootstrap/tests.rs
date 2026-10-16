use super::*;
use commonware_codec::Encode as _;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_utils::{Participant, TryCollect as _};
use outbe_consensus::{bls::bootstrap_dkg_for_participants, hybrid::VrfMaterialProvider};
use radicle::identity::doc::GetPayload as _;
use radicle::{crypto::Signer as _, storage::WriteStorage as _};

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
    let public_recovery = VrfMaterialProvider::new(7, dkg.polynomial.clone(), None);
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
            .recover_bootstrap_document(
                &public_recovery,
                &document,
                &state,
                &document_contributions[..2]
            )
            .is_err());
        let document_signature = material
            .recover_bootstrap_document(
                &public_recovery,
                &document,
                &state,
                &document_contributions,
            )
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
            .recover_bootstrap_identity(
                &public_recovery,
                &identity,
                &state,
                &contributions(&identity_frames),
            )
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
            .recover_bootstrap_refs(
                &public_recovery,
                &refs,
                &state,
                &contributions(&refs_frames),
            )
            .unwrap();
        assert_eq!(refs_signature, sign(refs.subject(), first));
        let (body, manifest) = refs.attach(&repo, &refs_signature).unwrap();
        assert_eq!(repo.raw().references().unwrap().count(), 0);
        assert_eq!(body.descriptor_hash, descriptor.hash().unwrap());
        assert_eq!(body.finalized_anchor_height, 0);
        assert_eq!(body.finalized_anchor_hash, authority.genesis_hash());
        assert_eq!(manifest.subjects().len(), 3);
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
        let receipt = crate::manager::FinalizedPublicationReceipt {
            block: crate::manager::FinalizedBlock {
                number: 15,
                hash: B256::repeat_byte(15),
            },
            receipt: outbe_radicleregistry::publication::PublicationReceipt {
                accepted_height: 14,
                receipt_hash: body.receipt_hash(),
                body: body.clone(),
            },
        };
        let verifier = super::super::signing::accepted::AcceptedPublicationVerifier::from_receipt(
            receipt.clone(),
            manifest.clone(),
            &material.canonical_output(),
        )
        .unwrap();
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
                &repo, &state, &material, b"[]", &body, &manifest, &witness,
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
        assert_eq!(
            super::super::network::material_tests::bootstrap_attestations(
                selected,
                &state,
                &dkg.output,
                &keys
            )
            .await
            .len(),
            4
        );
        let slot = super::super::publication::store::review_slot_key(&body);
        drop(store);
        let store =
            super::super::publication::store::ReviewPackageStore::open(&store_path).unwrap();
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
            &state,
            &material,
            b"[]",
            &loaded.body,
            &loaded.manifest,
            &loaded.witness,
        )
        .unwrap();
        assert_eq!(replay_repo.raw().references().unwrap().count(), 0);
        assert!(verify_accepted_bootstrap(&repo, &descriptor, b"[]", &verifier, &short).is_err());
        assert!(
            verify_accepted_bootstrap(&repo, &descriptor, b"[]\n", &verifier, &witness).is_err()
        );
        let proof =
            verify_accepted_bootstrap(&repo, &descriptor, b"[]", &verifier, &witness).unwrap();
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
        radicle::cob::object::Storage::update(
            &repo,
            &RepositoryKey::from(owner),
            &identity::TYPENAME,
            &identity.into(),
            &identity,
        )
        .unwrap();
        assert_eq!(
            radicle::cob::identity::Identity::get(&identity.into(), &repo)
                .unwrap()
                .doc()
                .encode()
                .unwrap()
                .0,
            git::Oid::from_sha1(repo.id().into_sha1().unwrap())
        );
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
