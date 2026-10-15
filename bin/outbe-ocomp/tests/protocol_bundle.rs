mod support;

use alloy_primitives::B256;
use outbe_ocomp::{bundle::PinnedProtocolBundle, control::poc_schema_limits};
use outbe_ocomp_protocol::ProtocolError;

#[test]
fn canonical_bundle_bytes_resolve_only_to_the_expected_supported_bundle() {
    let limits = poc_schema_limits();
    let bundle = support::protocol_bundle();
    let canonical = bundle.encode_canonical(&limits).unwrap();
    let hash = bundle.protocol_bundle_hash(&limits).unwrap();

    let pinned = PinnedProtocolBundle::decode(&canonical, hash, &limits).unwrap();
    assert_eq!(pinned.bundle(), &bundle);
    assert_eq!(pinned.hash(), hash);

    assert_eq!(
        PinnedProtocolBundle::decode(&canonical, B256::repeat_byte(0xee), &limits).unwrap_err(),
        ProtocolError::HashMismatch
    );

    let mut unsupported = bundle;
    unsupported.fidelity_opening_codec_id = B256::repeat_byte(0xef);
    let unsupported = unsupported.encode_canonical(&limits).unwrap();
    assert_eq!(
        PinnedProtocolBundle::decode(
            &unsupported,
            outbe_ocomp_protocol::profile::ProtocolBundleV1::decode_canonical(
                &unsupported,
                &limits
            )
            .unwrap()
            .protocol_bundle_hash(&limits)
            .unwrap(),
            &limits
        )
        .unwrap_err(),
        ProtocolError::InvalidInvariant("unsupported Lysis V1 input codec bundle")
    );
}

#[test]
fn old_execution_semantics_are_rejected_even_with_a_matching_bundle_hash() {
    let limits = poc_schema_limits();
    let mut bundle = support::protocol_bundle();
    bundle.lysis_program_semantics_hash = B256::repeat_byte(8);
    let canonical = bundle.encode_canonical(&limits).unwrap();
    let hash = bundle.protocol_bundle_hash(&limits).unwrap();
    let expected = ProtocolError::InvalidInvariant("unsupported Lysis V1 execution semantics");
    assert_eq!(
        PinnedProtocolBundle::decode(&canonical, hash, &limits).unwrap_err(),
        expected
    );
    assert_eq!(
        PinnedProtocolBundle::decode_canonical(&canonical, &limits).unwrap_err(),
        expected
    );
}
