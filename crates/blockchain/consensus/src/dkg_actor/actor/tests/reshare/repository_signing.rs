//! Repository signer evidence using interactive actors, not centralized bootstrap/dealing.

use super::*;
use crate::hybrid::{RepositorySigningError, VrfMaterialProvider};
use commonware_cryptography::bls12381::primitives::{ops, variant::PartialSignature};
use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
use commonware_utils::Participant;
use radicle_crypto::{BlsEnvelope, BlsPublicKey, RepositorySubject};

async fn interactive_genesis(
    context: &commonware_runtime::deterministic::Context,
    keys: &[bls12381::PrivateKey],
) -> Vec<DkgComplete> {
    let participants: Set<_> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let (senders, receivers) = build_mock_network(keys);
    let mut handles = Vec::new();
    for ((key, sender), receiver) in keys.iter().cloned().zip(senders).zip(receivers) {
        let participants = participants.clone();
        handles.push(
            context
                .child("repository_genesis_dkg")
                .spawn(move |clock| async move {
                    run_initial_dkg(
                        &clock,
                        InitialDkgFixture::bootstrap(key, participants),
                        None,
                        None,
                        (sender, receiver),
                    )
                    .await
                }),
        );
    }
    let mut complete = Vec::new();
    for handle in handles {
        complete.push(handle.await.unwrap().unwrap());
    }
    assert_agreement(&complete);
    complete
}

fn assert_agreement(results: &[DkgComplete]) {
    let expected = results[0].output.encode();
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result.output.encode(), expected);
        assert_eq!(result.share.index, Participant::from_usize(index));
        assert!(
            result.output.revealed().is_empty(),
            "all-online ceremony must not reveal player shares"
        );
    }
}

fn partials(
    providers: &[VrfMaterialProvider<MinSig>],
    version: u64,
    output: &Output<MinSig, bls12381::PublicKey>,
    subject: &RepositorySubject,
) -> Vec<PartialSignature<MinSig>> {
    providers
        .iter()
        .enumerate()
        .map(|(index, provider)| {
            provider
                .sign_repository(
                    version,
                    Participant::from_usize(index),
                    output.public(),
                    subject,
                )
                .unwrap()
        })
        .collect()
}

#[test]
fn interactive_dkg_and_membership_reshare_feed_native_repository_signing() {
    // Use the existing DKG test runner bound; production ceremony code and wire run over a mock mesh.
    commonware_runtime::deterministic::Runner::timed(Duration::from_secs(600))
        .start(repository_signing_scenario);
}

async fn repository_signing_scenario(context: commonware_runtime::deterministic::Context) {
    let mut old_keys: Vec<_> = (301..305).map(bls12381::PrivateKey::from_seed).collect();
    old_keys.sort_by_key(|key| key.public_key());
    let initial = interactive_genesis(&context, &old_keys).await;
    let old_output = initial[0].output.clone();
    let old_participants = initial[0].participants.clone();
    let old_shares: Vec<_> = initial.iter().map(|result| result.share.clone()).collect();
    let providers: Vec<_> = initial
        .iter()
        .map(|result| {
            VrfMaterialProvider::new(
                7,
                result.output.public().clone(),
                Some(result.share.clone()),
            )
        })
        .collect();
    // Framing/signing evidence; the exact Git Review bytes are covered by adapter preparation tests.
    let historical =
        RepositorySubject::cob([1; 20], [7; 32], [3; 20], b"native Review Accept bytes");
    let old_partials = partials(&providers, 7, &old_output, &historical);
    let old_verifier = VrfMaterialProvider::new(7, old_output.public().clone(), None);
    let historical_signature = old_verifier
        .recover_repository(7, old_output.public(), &historical, &old_partials[..3])
        .unwrap();
    assert_eq!(
        historical_signature,
        old_verifier
            .recover_repository(7, old_output.public(), &historical, &old_partials[1..])
            .unwrap()
    );
    assert_eq!(
        old_verifier.recover_repository(7, old_output.public(), &historical, &old_partials[..2]),
        Err(RepositorySigningError::InsufficientPartials)
    );

    let removed = old_keys[0].public_key();
    let newcomer = bls12381::PrivateKey::from_seed(1001);
    let mut target_keys = old_keys[1..].to_vec();
    target_keys.push(newcomer.clone());
    target_keys.sort_by_key(|key| key.public_key());
    let target: Set<_> = target_keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let mut union = old_keys.clone();
    union.push(newcomer);
    union.sort_by_key(|key| key.public_key());
    let verification = LogVerification::new(&old_participants, &old_output, &target).unwrap();
    let (
        Players {
            mut progress_rx,
            finalized_log_txs,
            handles,
        },
        dealer_only,
    ) = spawn_with_removed_dealer(
        &context,
        PlayerCommittee {
            keys: &union,
            participants: &target,
            previous: PreviousShares {
                keys: &old_keys,
                output: &old_output,
                shares: &old_shares,
            },
        },
        &removed,
        build_mock_network(&union),
    );
    let selected = verification
        .collect(
            &mut progress_rx,
            LogSelection {
                threshold: old_participants.quorum::<N3f1>() as usize,
                required: Some(&removed),
            },
            |_| true,
        )
        .await
        .unwrap();
    // This harness injects a canonical selected log set; it does not run chain consensus/finality.
    for log in selected.values() {
        for tx in &finalized_log_txs {
            tx.send(log.clone()).unwrap();
        }
    }
    let mut refreshed = Vec::new();
    for handle in handles {
        refreshed.push(handle.await.unwrap().unwrap());
    }
    dealer_only.unwrap().await.unwrap().unwrap();
    assert_agreement(&refreshed);
    let output = refreshed[0].output.clone();
    assert_eq!(output.players(), &target);
    assert_eq!(
        output.public().public(),
        old_output.public().public(),
        "normal reshare keeps repository owner key"
    );
    assert_ne!(
        output.public().encode(),
        old_output.public().encode(),
        "reshare must change the public polynomial"
    );
    assert!(output.players().position(&removed).is_none());
    for (index, key) in old_keys.iter().enumerate() {
        let share = target_keys
            .iter()
            .position(|target_key| target_key.public_key() == key.public_key())
            .map(|position| refreshed[position].share.clone());
        providers[index].activate(8, output.public().clone(), share);
        assert_eq!(
            providers[index].sign_repository(
                7,
                Participant::from_usize(index),
                old_output.public(),
                &historical
            ),
            Err(RepositorySigningError::InactiveMaterial)
        );
    }
    assert_eq!(
        providers[0].sign_repository(8, Participant::new(0), output.public(), &historical),
        Err(RepositorySigningError::MissingShare)
    );
    let current_providers: Vec<_> = target_keys
        .iter()
        .enumerate()
        .map(|(index, key)| {
            old_keys
                .iter()
                .position(|old_key| old_key.public_key() == key.public_key())
                .map(|old_index| providers[old_index].clone())
                .unwrap_or_else(|| {
                    VrfMaterialProvider::new(
                        8,
                        output.public().clone(),
                        Some(refreshed[index].share.clone()),
                    )
                })
        })
        .collect();
    let current = RepositorySubject::cob([1; 20], [8; 32], [3; 20], b"native Review Accept bytes");
    let contributions = partials(&current_providers, 8, &output, &current);
    let verifier = VrfMaterialProvider::new(8, output.public().clone(), None);
    let signature = verifier
        .recover_repository(8, output.public(), &current, &contributions[..3])
        .unwrap();
    assert_eq!(
        signature,
        verifier
            .recover_repository(8, output.public(), &current, &contributions[1..])
            .unwrap()
    );
    assert_eq!(
        verifier.recover_repository(8, output.public(), &current, &contributions[..2]),
        Err(RepositorySigningError::InsufficientPartials)
    );
    assert_eq!(
        verifier.recover_repository(8, output.public(), &current, &old_partials[..3]),
        Err(RepositorySigningError::InvalidPartial)
    );
    let mixed = vec![
        old_partials[0].clone(),
        contributions[1].clone(),
        contributions[2].clone(),
    ];
    assert_eq!(
        verifier.recover_repository(8, output.public(), &current, &mixed),
        Err(RepositorySigningError::InvalidPartial)
    );
    assert_eq!(
        verifier.recover_repository(8, old_output.public(), &historical, &old_partials[..3]),
        Err(RepositorySigningError::PolynomialMismatch)
    );
    let owner = BlsPublicKey::from_bytes(
        output
            .public()
            .public()
            .encode()
            .as_ref()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    let historical_envelope = BlsEnvelope::attach(
        historical.clone(),
        owner,
        historical_signature.encode().as_ref().try_into().unwrap(),
    )
    .unwrap();
    historical_envelope
        .verify(&historical, owner.into())
        .unwrap();
    let envelope = BlsEnvelope::attach(
        current.clone(),
        owner,
        signature.encode().as_ref().try_into().unwrap(),
    )
    .unwrap();
    envelope.verify(&current, owner.into()).unwrap();
    assert!(ops::verify_message::<MinSig>(
        output.public().public(),
        radicle_crypto::repository::DOMAIN,
        historical.as_bytes(),
        &historical_signature
    )
    .is_ok());
    assert!(ops::verify_message::<MinSig>(
        output.public().public(),
        b"outbe.radicle.partial.v1",
        current.as_bytes(),
        &signature
    )
    .is_err());
    eprintln!(
        "{}",
        serde_json::json!({
            "scenario":"interactive-dkg-then-membership-reshare-repository-signing",
            "transport":"mock in-memory mesh; real durable actors/wire; injected canonical reshare logs",
            "owner_public":hex::encode(owner.as_bytes()),
            "initial_public_polynomial":hex::encode(old_output.public().encode()),
            "reshared_public_polynomial":hex::encode(output.public().encode()),
            "removed_identity_public":hex::encode(removed.encode()),
            "current_identity_public":target_keys.iter().map(|key| hex::encode(key.public_key().encode())).collect::<Vec<_>>(),
            "historical_subject":hex::encode(historical.as_bytes()),
            "historical_envelope":historical_envelope.to_hex(),
            "current_subject":hex::encode(current.as_bytes()),
            "current_envelope":envelope.to_hex(),
            "q":3,"subsets":[[0,1,2],[1,2,3]],"initial_revealed":0,"reshared_revealed":0,
            "initial_partials":old_partials.iter().map(|partial| serde_json::json!({"index":partial.index.get(),"signature":hex::encode(partial.value.encode())})).collect::<Vec<_>>(),
            "current_partials":contributions.iter().map(|partial| serde_json::json!({"index":partial.index.get(),"signature":hex::encode(partial.value.encode())})).collect::<Vec<_>>(),
        })
    );
}
