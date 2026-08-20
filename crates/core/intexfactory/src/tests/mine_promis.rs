use super::*;

#[test]
fn settled_token_id_derivation() {
    // uint256(keccak256("SETTLED" ++ seriesId_be64))
    let series_id = sid(7);
    let mut buf = Vec::new();
    buf.extend_from_slice(b"SETTLED");
    buf.extend_from_slice(series_id.as_bytes());
    assert_eq!(
        runtime::settled_token_id(series_id),
        U256::from_be_bytes(keccak256(&buf).0)
    );
}

#[test]
fn compute_pow_hash_matches_manual_sha256() {
    // SHA256(hex(holder)++hex(promisAmount)++hex(seriesId)++hex(seq) ++ nonce_be8)
    let promis_amount = U256::from(1_000u64);
    let (series_id, seq, nonce) = (sid(7), 3u32, 42u64);
    let got = runtime::compute_pow_hash(holder(), promis_amount, series_id, seq, U256::from(nonce))
        .unwrap();

    let mut preimage = String::new();
    preimage.push_str(&hex::encode(holder().as_slice()));
    preimage.push_str(&hex::encode(promis_amount.to_be_bytes::<32>()));
    preimage.push_str(&hex::encode(series_id.as_bytes()));
    preimage.push_str(&hex::encode(seq.to_be_bytes()));
    let mut data = preimage.into_bytes();
    data.extend_from_slice(&nonce.to_be_bytes());
    let expected = ring::digest::digest(&ring::digest::SHA256, &data);
    assert_eq!(got.as_slice(), expected.as_ref());
}

#[test]
fn validate_pow_accepts_valid_and_rejects_invalid_nonce() {
    let pa = U256::from(1_000u64);
    let (series_id, seq) = (sid(7), 0u32);
    // Difficulty 1: ~1/256 of nonces pass; brute-force a valid and an invalid one.
    let mut good = None;
    let mut bad = None;
    for n in 0u64..100_000 {
        let ok = runtime::validate_pow(holder(), pa, series_id, seq, U256::from(n)).is_ok();
        if ok && good.is_none() {
            good = Some(n);
        }
        if !ok && bad.is_none() {
            bad = Some(n);
        }
        if good.is_some() && bad.is_some() {
            break;
        }
    }
    assert!(runtime::validate_pow(
        holder(),
        pa,
        series_id,
        seq,
        U256::from(good.expect("a valid nonce"))
    )
    .is_ok());
    assert!(runtime::validate_pow(
        holder(),
        pa,
        series_id,
        seq,
        U256::from(bad.expect("an invalid nonce"))
    )
    .is_err());
}

#[test]
fn validate_pow_rejects_nonce_over_u64() {
    assert!(runtime::validate_pow(
        holder(),
        U256::from(1u64),
        sid(1),
        0,
        U256::from(u64::MAX) + U256::from(1)
    )
    .is_err());
}

/// A dummy authorization for mine_promis paths that reject before the (enclave)
/// Promis mint (zero amount / missing series).
fn no_auth() -> outbe_promisfactory::api::ModifyAuth {
    outbe_promisfactory::api::ModifyAuth {
        mac: [0u8; 32],
        op_nonce: 0,
    }
}

#[test]
fn mine_promis_rejects_zero_amount() {
    with_factory(|s| {
        assert!(
            runtime::mine_promis(&s, sid(7), holder(), U256::ZERO, U256::ZERO, no_auth()).is_err()
        );
    });
}

#[test]
fn mine_promis_rejects_missing_series() {
    with_factory(|s| {
        assert!(
            runtime::mine_promis(&s, sid(7), holder(), U256::from(1), U256::ZERO, no_auth())
                .is_err()
        );
    });
}
