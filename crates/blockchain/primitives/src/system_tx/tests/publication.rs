use super::*;
use crate::{
    radicle_publication::tests::batch,
    reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts},
    system_tx::binding::validate_system_layout,
    OutbeBlockBody, OutbeHeader,
};

#[test]
fn publication_calldata_is_bounded_and_canonical() {
    let input = SystemTx::RadiclePublication { batch: batch(32) };
    let encoded = input.encode().unwrap();
    assert_eq!(&encoded[..5], b"OSP2\x02");
    assert_eq!(encoded.len(), 3683);
    assert_eq!(SystemTx::decode(&encoded).unwrap(), input);
    let mut trailing = encoded.to_vec();
    trailing.push(0);
    assert!(SystemTx::decode(&trailing).is_err());
    assert!(SystemTx::decode(&encoded[..encoded.len() - 1]).is_err());
    let mut old = encoded.to_vec();
    old[4] = 1;
    assert!(SystemTx::decode(&old).is_err());
    assert!(input.revert_fails_block());
}

fn transaction(input: SystemTx, index: usize) -> TransactionSigned {
    build_unsigned_system_tx(index as u8, 2, CHAIN_ID, input.encode().unwrap())
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
    let selectors = expected_begin_block_selectors_for_publication(2, false, false, true);
    assert_eq!(
        &selectors[selectors.len() - 2..],
        &[HOOK_EVENTS_SELECTOR, RADICLE_PUBLICATION_SELECTOR]
    );
    let inputs = [
        accounting(),
        late_credits(),
        SystemTx::CycleTick,
        SystemTx::OracleSlashWindow,
        SystemTx::HookEvents,
        SystemTx::RadiclePublication { batch: batch(32) },
    ];
    let mut body = OutbeBlockBody {
        transactions: inputs
            .into_iter()
            .enumerate()
            .map(|(index, input)| transaction(input, index))
            .collect(),
        ..Default::default()
    };
    body.transactions.extend([
        transaction(
            SystemTx::CallEventProcessing,
            CALL_EVENT_PROCESSING_ORDINAL as usize,
        ),
        transaction(
            SystemTx::CleanupProcessing,
            CLEANUP_PROCESSING_ORDINAL as usize,
        ),
    ]);
    assert!(validate_system_layout(&body, &header).is_ok());
    let publication_index = body.transactions.len() - 3;
    let accepted = body.transactions.remove(publication_index);
    assert!(validate_system_layout(&body, &header).is_err());
    let mut different = batch(32);
    different.body.package_manifest_hash = B256::repeat_byte(99);
    body.transactions.insert(
        publication_index,
        transaction(
            SystemTx::RadiclePublication { batch: different },
            publication_index,
        ),
    );
    assert!(validate_system_layout(&body, &header).is_err());
    body.transactions[publication_index] = accepted;
    let mut no_batch = header.clone();
    no_batch.inner.extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        ..Default::default()
    })
    .unwrap();
    assert!(validate_system_layout(&body, &no_batch).is_err());
    body.transactions.remove(publication_index);
    assert!(validate_system_layout(&body, &no_batch).is_ok());
    no_batch.inner.extra_data = Bytes::new();
    assert!(validate_system_layout(&body, &no_batch).is_ok());
}

#[test]
fn publication_cursor_advances_only_after_hooks_when_a_batch_is_present() {
    let hooks = SystemTxPhase::HookEvents { body_index: 8 };
    assert_eq!(
        hooks.advance_after_commit(false, false),
        SystemTxPhase::UserTxs
    );
    let publication = hooks.advance_after_commit_with_publication(false, false, true);
    assert_eq!(
        publication,
        SystemTxPhase::RadiclePublication { body_index: 9 }
    );
    assert_eq!(
        publication.expected_selector(),
        Some(RADICLE_PUBLICATION_SELECTOR)
    );
    assert_eq!(
        publication.advance_after_commit_with_publication(false, false, true),
        SystemTxPhase::UserTxs
    );
}
