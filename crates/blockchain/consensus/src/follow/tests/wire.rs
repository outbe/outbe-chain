//! Finalized-delivery wire format and public finalization decoding.

use super::*;

/// The follow resolver serves a `Request::Finalized` delivery as the
/// finalization certificate bytes immediately followed by the block bytes.
/// The marshal decodes that exact layout by reading the `Finalization` with
/// the epoch verifier's certificate codec config, then decoding the
/// `ConsensusBlock` from the REMAINING buffer. This pins that two-step decode
/// against the resolver's `finalization.encode() ++ block.encode()` wire
/// format - the load-bearing interop contract between the follower's
/// resolver and the marshal (a divergence here would compile clean but fail
/// every backfill at runtime).
#[test]
fn finalized_delivery_wire_format_round_trips() {
    use crate::block::ConsensusBlock;
    use commonware_codec::Read as _;
    use commonware_cryptography::certificate::Verifier as _;

    let epoch = Epoch::new(3);
    let c = committee(20);

    // A certificate the marshal will decode with this verifier's config.
    let finalization = c.finalization(epoch);
    let verifier = HybridScheme::<MinSig>::verifier(
        &crate::config::outbe_app_namespace(),
        c.participants.clone(),
        c.dkg.polynomial.clone(),
    )
    .unwrap();
    let cert_cfg = verifier.certificate_codec_config();

    // An arbitrary valid block (its digest need not match the finalization
    // payload for the codec contract - the marshal checks that separately).
    let block = {
        use alloy_primitives::Bytes;
        use outbe_primitives::OutbeHeader;
        use reth_ethereum::primitives::SealedBlock;
        use reth_ethereum::Block;
        let mut b = Block::default();
        b.header.number = 42;
        b.header.extra_data = Bytes::from_static(b"wire-fmt");
        let b = b.map_header(OutbeHeader::new);
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(b))
    };

    // Exactly what `resolver::resolve_one` builds for a Finalized delivery.
    let mut wire = finalization.encode().to_vec();
    wire.extend_from_slice(block.encode().as_ref());

    // Decode the marshal's way: certificate first (with its cfg), block from
    // the remaining bytes.
    let mut buf: &[u8] = &wire;
    let decoded_fin = Finalization::<HybridScheme<MinSig>, Digest>::read_cfg(&mut buf, &cert_cfg)
        .expect("finalization must decode from the delivery prefix");
    let decoded_block = ConsensusBlock::read_cfg(&mut buf, &())
        .expect("block must decode from the delivery suffix");

    assert_eq!(
        decoded_fin.proposal.payload, finalization.proposal.payload,
        "decoded finalization payload must match"
    );
    assert_eq!(
        decoded_block.digest(),
        block.digest(),
        "decoded block digest must match the served block"
    );
    assert!(
        buf.is_empty(),
        "the delivery buffer must be fully consumed (cert ++ block, nothing trailing)"
    );
}

/// Full `outbe_getFinalization` server->client interop. The SERVER side
/// (drainer) encodes the certificate and block separately and hexes them
/// (`FinalizedBlockBytes` -> `FinalizationProof`); the CLIENT side hex-decodes
/// and decodes the certificate with the UNBOUNDED committee config (the
/// engine `UpstreamRpcClient` path - it has no committee size yet), then the
/// follower registers the epoch committee from the boundary block and the
/// marshal-equivalent verification passes. This pins that:
///   (a) the unbounded cfg decodes a real committee-length certificate, and
///   (b) the decoded `(finalization, block)` is exactly what the resolver
///       registers + the `CommitteeChain` verifies - i.e. a follower accepts
///       what a validator serves, end to end.
#[test]
fn served_finalization_round_trips_to_verified_certified_block() {
    use crate::block::ConsensusBlock;
    use commonware_codec::Read as _;
    use commonware_cryptography::certificate::Verifier as _;

    let epoch = Epoch::new(4);
    let c = committee(40);

    // Anchor a chain on this committee and register epoch 4 from its boundary
    // block - exactly what the follower does on the fetch path.
    let mut chain = CommitteeChain::new(epoch, c.participants.clone());
    let boundary_extra = c.boundary_block_extra_data(epoch);
    assert_eq!(
        chain
            .advance_from_block_extra_data(&boundary_extra)
            .unwrap(),
        Some(epoch)
    );

    // SERVER: encode cert + block separately (the drainer's FinalizedBlockBytes)
    // and hex them (the FinalizationProof shipped over RPC).
    let finalization = c.finalization(epoch);
    let block = {
        use alloy_primitives::Bytes;
        use outbe_primitives::OutbeHeader;
        use reth_ethereum::primitives::SealedBlock;
        use reth_ethereum::Block;
        let mut b = Block::default();
        b.header.number = 4;
        b.header.extra_data = Bytes::from(boundary_extra.clone());
        let b = b.map_header(OutbeHeader::new);
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(b))
    };
    let finalization_hex = format!("0x{}", hex::encode(finalization.encode()));
    let block_hex = format!("0x{}", hex::encode(block.encode()));

    // CLIENT: hex-decode and decode the certificate with the UNBOUNDED
    // committee config (the engine UpstreamRpcClient path).
    let fin_bytes = hex::decode(finalization_hex.trim_start_matches("0x")).unwrap();
    let block_bytes = hex::decode(block_hex.trim_start_matches("0x")).unwrap();
    let unbounded_cfg = HybridScheme::<MinSig>::certificate_codec_config_unbounded();
    let mut fin_reader: &[u8] = &fin_bytes;
    let decoded_fin =
        Finalization::<HybridScheme<MinSig>, Digest>::read_cfg(&mut fin_reader, &unbounded_cfg)
            .expect("client must decode the served finalization with the unbounded cfg");
    assert!(
        fin_reader.is_empty(),
        "no trailing bytes after finalization"
    );
    let mut block_reader: &[u8] = &block_bytes;
    let _decoded_block = ConsensusBlock::read_cfg(&mut block_reader, &())
        .expect("client must decode the served block");
    assert!(block_reader.is_empty(), "no trailing bytes after block");

    // The decoded certificate verifies against the committee the follower
    // registered from the boundary block - a follower accepts what the
    // validator served.
    chain
        .verify_finalization(epoch, &decoded_fin)
        .expect("the round-tripped certificate must verify against the registered committee");
}

#[test]
fn public_finalization_decoder_requires_exact_canonical_record() {
    use commonware_codec::Encode as _;

    let committee = committee(44);
    let finalization = committee.finalization(Epoch::new(7));
    let encoded = finalization.encode();

    let decoded = decode_public_finalization(&encoded, committee.participants.len()).unwrap();
    assert_eq!(decoded.proposal, finalization.proposal);

    let mut trailing = encoded.to_vec();
    trailing.push(0);
    assert!(matches!(
        decode_public_finalization(&trailing, committee.participants.len()),
        Err(PublicFinalizedBlockDecodeError::TrailingFinalization)
    ));
    assert!(matches!(
        decode_public_finalization(&encoded, 0),
        Err(PublicFinalizedBlockDecodeError::ZeroCommitteeBound)
    ));
}
