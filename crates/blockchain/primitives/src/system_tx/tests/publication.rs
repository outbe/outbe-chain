use super::*;
use crate::{
    radicle_publication::tests::batch,
    reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts},
    system_tx::binding::validate_system_layout,
    OutbeBlockBody, OutbeHeader,
};

#[test]
fn publication_calldata_is_bounded_and_canonical() {
    let input = SystemTxInputV2::RadiclePublication { batch: batch(32) };
    let encoded = input.encode().unwrap();
    assert_eq!(&encoded[..5], b"OSP2\x02");
    assert_eq!(encoded.len(), 3683);
    assert_eq!(SystemTxInputV2::decode(&encoded).unwrap(), input);
    let mut trailing = encoded.to_vec();
    trailing.push(0);
    assert!(SystemTxInputV2::decode(&trailing).is_err());
    assert!(SystemTxInputV2::decode(&encoded[..encoded.len() - 1]).is_err());
    let mut old = encoded.to_vec();
    old[4] = 1;
    assert!(SystemTxInputV2::decode(&old).is_err());
    assert!(SystemTxKind::RadiclePublication.revert_fails_block());
}

fn transaction(input: SystemTxInputV2, index: usize) -> TransactionSigned {
    build_unsigned_system_tx(
        input.kind(),
        index as u8,
        2,
        CHAIN_ID,
        input.encode().unwrap(),
    )
    .unwrap()
    .into_signed(Signature::test_signature())
    .into()
}

#[test]
fn publication_layout_binds_exact_header_batch_after_hooks_and_preserves_legacy() {
    let artifacts = OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        radicle_publication: Some(batch(32)),
        ..Default::default()
    };
    let header = OutbeHeader::new(alloy_consensus::Header {
        number: 2,
        extra_data: encode_outbe_block_artifacts(&artifacts).unwrap(),
        ..Default::default()
    });
    let kinds = expected_begin_block_kinds_for_publication(
        2,
        false,
        false,
        OcompLifecycleActivation::Disabled,
        true,
    );
    assert_eq!(
        &kinds[kinds.len() - 2..],
        &[SystemTxKind::HookEvents, SystemTxKind::RadiclePublication]
    );
    let inputs: Vec<_> = kinds
        .into_iter()
        .map(|kind| {
            if kind == SystemTxKind::RadiclePublication {
                SystemTxInputV2::RadiclePublication { batch: batch(32) }
            } else {
                input_for(kind)
            }
        })
        .collect();
    let mut body = OutbeBlockBody {
        transactions: inputs
            .into_iter()
            .enumerate()
            .map(|(index, input)| transaction(input, index))
            .collect(),
        ..Default::default()
    };
    assert!(validate_system_layout(&body, &header, OcompLifecycleActivation::Disabled).is_ok());
    let accepted = body.transactions.pop().unwrap();
    assert!(validate_system_layout(&body, &header, OcompLifecycleActivation::Disabled).is_err());
    let mut different = batch(32);
    different.body.package_manifest_hash = B256::repeat_byte(99);
    body.transactions.push(transaction(
        SystemTxInputV2::RadiclePublication { batch: different },
        body.transactions.len(),
    ));
    assert!(validate_system_layout(&body, &header, OcompLifecycleActivation::Disabled).is_err());
    *body.transactions.last_mut().unwrap() = accepted;
    let mut no_batch = header.clone();
    no_batch.inner.extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        ..Default::default()
    })
    .unwrap();
    assert!(validate_system_layout(&body, &no_batch, OcompLifecycleActivation::Disabled).is_err());
    body.transactions.pop();
    assert!(validate_system_layout(&body, &no_batch, OcompLifecycleActivation::Disabled).is_ok());
    no_batch.inner.extra_data = Bytes::new();
    assert!(validate_system_layout(&body, &no_batch, OcompLifecycleActivation::Disabled).is_ok());
}

#[test]
fn publication_cursor_advances_only_after_hooks_when_a_batch_is_present() {
    let hooks = SystemTxPhase::HookEvents { body_index: 8 };
    assert_eq!(
        hooks.advance_after_commit_with_ocomp(false, false, false),
        SystemTxPhase::UserTxs
    );
    let publication = hooks.advance_after_commit_with_publication(false, false, false, true);
    assert_eq!(
        publication,
        SystemTxPhase::RadiclePublication { body_index: 9 }
    );
    assert_eq!(
        publication.expected_kind(),
        Some(SystemTxKind::RadiclePublication)
    );
    assert_eq!(
        publication.advance_after_commit_with_publication(false, false, false, true),
        SystemTxPhase::UserTxs
    );
}
