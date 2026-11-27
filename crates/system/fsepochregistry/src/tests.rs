use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolError};
use blst::min_sig::SecretKey;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::bls::{encode_eip2537_g1, encode_eip2537_g2, verify_eip2537_min_sig};
use crate::precompile::{dispatch, IFsEpochRegistry};
use crate::runtime::{digest_of, FsEpochHead};
use crate::schema::FsEpochRegistryContract;

const ADMIN: Address = Address::repeat_byte(0xAA);
const ANYONE: Address = Address::repeat_byte(0xBB);

fn keypair(seed_byte: u8) -> (SecretKey, Bytes) {
    let ikm = [seed_byte; 32];
    let sk = SecretKey::key_gen(&ikm, &[]).expect("key_gen");
    let pk = sk.sk_to_pk();
    (sk, Bytes::from(encode_eip2537_g2(&pk).to_vec()))
}

fn sign(sk: &SecretKey, msg: &[u8], dst: &[u8]) -> Bytes {
    let sig = sk.sign(msg, dst, &[]);
    Bytes::from(encode_eip2537_g1(&sig).to_vec())
}

fn revert_selector(err: PrecompileError) -> [u8; 4] {
    match err {
        PrecompileError::RevertBytes(bytes) if bytes.len() >= 4 => {
            bytes[..4].try_into().expect("selector")
        }
        other => panic!("expected typed revert, got {other:?}"),
    }
}

/// Pins `digest_of` against the cross-repo `fs_epoch_parity.json` vectors shared with
/// `bank-data-provider` (`crates/bdp-seal-codec/tests/fixtures/fs_epoch_parity.json`).
#[test]
fn digest_matches_bdp_parity_vectors() {
    let cases: &[(u32, &str, &str, u32, &str, &str, &str)] = &[
        (
            0,
            "000000000000000000000000000000003336393c3f4245484b4e5154575a5d606366696c6f7275787b7e8184878a8d909396999c9fa2a5a8abaeb1b4b7babdc000000000000000000000000000000000f3f6f9fcff0205080b0e1114171a1d202326292c2f3235383b3e4144474a4d505356595c5f6265686b6e7174777a7d8000000000000000000000000000000000b3b6b9bcbfc2c5c8cbced1d4d7dadde0e3e6e9eceff2f5f8fbfe0104070a0d101316191c1f2225282b2e3134373a3d40000000000000000000000000000000007376797c7f8285888b8e9194979a9da0a3a6a9acafb2b5b8bbbec1c4c7cacdd0d3d6d9dcdfe2e5e8ebeef1f4f7fafd00",
            "50534f2d46532d4154542d5631",
            20260101,
            "1111111111111111111111111111111111111111111111111111111111111111",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "05eb66bb4845368f5c4cf305d1871db55de17ef89aac232b15c9aa3127b6246a",
        ),
        (
            1,
            "00000000000000000000000000000000555a5f64696e73787d82878c91969ba0a5aaafb4b9bec3c8cdd2d7dce1e6ebf0f5faff04090e13181d22272c31363b4000000000000000000000000000000000959a9fa4a9aeb3b8bdc2c7ccd1d6dbe0e5eaeff4f9fe03080d12171c21262b30353a3f44494e53585d62676c71767b8000000000000000000000000000000000d5dadfe4e9eef3f8fd02070c11161b20252a2f34393e43484d52575c61666b70757a7f84898e93989da2a7acb1b6bbc000000000000000000000000000000000151a1f24292e33383d42474c51565b60656a6f74797e83888d92979ca1a6abb0b5babfc4c9ced3d8dde2e7ecf1f6fb00",
            "01",
            20260401,
            "2222222222222222222222222222222222222222222222222222222222222222",
            "05eb66bb4845368f5c4cf305d1871db55de17ef89aac232b15c9aa3127b6246a",
            "3c05de032ac744bd36939b9d93c3f65574c6c1d271bf9120b7e469033d125152",
        ),
        (
            3,
            "00000000000000000000000000000000bbc6d1dce7f2fd08131e29343f4a55606b76818c97a2adb8c3ced9e4effa05101b26313c47525d68737e89949faab5c0000000000000000000000000000000007b86919ca7b2bdc8d3dee9f4ff0a15202b36414c57626d78838e99a4afbac5d0dbe6f1fc07121d28333e49545f6a7580000000000000000000000000000000003b46515c67727d88939ea9b4bfcad5e0ebf6010c17222d38434e59646f7a85909ba6b1bcc7d2dde8f3fe09141f2a354000000000000000000000000000000000fb06111c27323d48535e69747f8a95a0abb6c1ccd7e2edf8030e19242f3a45505b66717c87929da8b3bec9d4dfeaf500",
            "424c535f5349475f424c53313233383147315f584d443a5348412d3235365f535357555f524f5f4e554c5f",
            20261001,
            "4444444444444444444444444444444444444444444444444444444444444444",
            "ff3510c7644d55eeff4171361de6fa5b384d14ee2ec38616a0c8752ee2af5e46",
            "90c26a6bc88e04a7ee2d60f36fd5e07aebe780f75c4dc3494c8e7a8c4e686f4d",
        ),
    ];

    for (epoch, pk_hex, dst_hex, start_wwd, members_hex, prev_hex, expected_hex) in cases {
        let pk = hex::decode(pk_hex).unwrap();
        let dst = hex::decode(dst_hex).unwrap();
        let members = B256::from_slice(&hex::decode(members_hex).unwrap());
        let prev = B256::from_slice(&hex::decode(prev_hex).unwrap());
        let expected = B256::from_slice(&hex::decode(expected_hex).unwrap());
        let actual = digest_of(*epoch, &pk, &dst, *start_wwd, members, prev).unwrap();
        assert_eq!(actual, expected, "epoch {epoch} parity mismatch");
    }
}

#[test]
fn bls_eip2537_sign_and_verify_roundtrip() {
    let (sk, pk) = keypair(7);
    let dst = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_";
    let msg = b"hello outbe fs epoch";
    let sig = sign(&sk, msg, dst);
    assert!(verify_eip2537_min_sig(&pk, msg, &sig, dst));
    assert!(!verify_eip2537_min_sig(&pk, b"tampered", &sig, dst));
    assert!(!verify_eip2537_min_sig(&pk, msg, &sig, b"other-dst"));
    assert!(!verify_eip2537_min_sig(&[0u8; 256], msg, &[0u8; 128], dst));
}

#[test]
fn anchor_advance_truncate_and_head_info() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut reg = FsEpochRegistryContract::new(storage.clone());
        reg.bootstrap_admin(ADMIN).unwrap();

        assert_eq!(reg.head_info().unwrap(), FsEpochHead::default());

        let (sk0, pk0) = keypair(10);
        let dst0 = Bytes::from_static(b"PSO-FS-ATT-V1");
        let ep0 = IFsEpochRegistry::Epoch {
            pkAtt: pk0.clone(),
            dst: dst0.clone(),
            startWwd: 20260101,
            membersHash: B256::repeat_byte(0x01),
            prevHash: B256::ZERO,
        };
        let d0 = digest_of(0, &pk0, &dst0, ep0.startWwd, ep0.membersHash, ep0.prevHash).unwrap();

        // Non-admin anchor fails.
        let err = dispatch(
            storage.clone(),
            &IFsEpochRegistry::anchorCall {
                epoch0: ep0.clone(),
            }
            .abi_encode(),
            ANYONE,
            U256::ZERO,
        )
        .unwrap_err();
        assert_eq!(revert_selector(err), IFsEpochRegistry::NotAdmin::SELECTOR);

        // Admin anchors epoch 0.
        dispatch(
            storage.clone(),
            &IFsEpochRegistry::anchorCall {
                epoch0: ep0.clone(),
            }
            .abi_encode(),
            ADMIN,
            U256::ZERO,
        )
        .unwrap();

        assert_eq!(
            reg.head_info().unwrap(),
            FsEpochHead {
                anchored: true,
                head: 0,
                start_wwd: 20260101,
                digest: d0,
            }
        );

        // Advance epochs 1 and 2 from ANYONE, replaying epoch 0 in prefix.
        let (sk1, pk1) = keypair(11);
        let dst1 = Bytes::from_static(b"PSO-FS-ATT-V1-E1");
        let ep1 = IFsEpochRegistry::Epoch {
            pkAtt: pk1.clone(),
            dst: dst1.clone(),
            startWwd: 20260108,
            membersHash: B256::repeat_byte(0x02),
            prevHash: d0,
        };
        let d1 = digest_of(1, &pk1, &dst1, ep1.startWwd, ep1.membersHash, ep1.prevHash).unwrap();
        let sig1 = sign(&sk0, d1.as_slice(), &dst0);

        let (_sk2, pk2) = keypair(12);
        let dst2 = Bytes::from_static(b"PSO-FS-ATT-V1-E2");
        let ep2 = IFsEpochRegistry::Epoch {
            pkAtt: pk2.clone(),
            dst: dst2.clone(),
            startWwd: 20260115,
            membersHash: B256::repeat_byte(0x03),
            prevHash: d1,
        };
        let d2 = digest_of(2, &pk2, &dst2, ep2.startWwd, ep2.membersHash, ep2.prevHash).unwrap();
        let sig2 = sign(&sk1, d2.as_slice(), &dst1);

        dispatch(
            storage.clone(),
            &IFsEpochRegistry::advanceCall {
                chain: vec![
                    IFsEpochRegistry::SignedEpoch {
                        epoch: 0,
                        entry: ep0.clone(),
                        sigPrev: Bytes::new(),
                    },
                    IFsEpochRegistry::SignedEpoch {
                        epoch: 1,
                        entry: ep1.clone(),
                        sigPrev: sig1.clone(),
                    },
                    IFsEpochRegistry::SignedEpoch {
                        epoch: 2,
                        entry: ep2.clone(),
                        sigPrev: sig2.clone(),
                    },
                ],
            }
            .abi_encode(),
            ANYONE,
            U256::ZERO,
        )
        .unwrap();

        assert_eq!(reg.head.read().unwrap(), 2);
        let k1 = reg.key_for(1).unwrap();
        assert_eq!(k1.start_wwd, 20260108);
        assert_eq!(k1.end_wwd_exclusive, 20260115);
        let k2 = reg.key_for(2).unwrap();
        assert_eq!(k2.end_wwd_exclusive, 0);

        // Truncate from 2: head becomes 1, adminNext becomes true, epoch 2 unreadable.
        reg.truncate(ADMIN, 2).unwrap();
        assert_eq!(reg.head.read().unwrap(), 1);
        assert!(reg.admin_next.read().unwrap());
        assert_eq!(
            revert_selector(reg.key_for(2).unwrap_err()),
            IFsEpochRegistry::EpochUnknown::SELECTOR
        );

        // Non-admin cannot re-advance after truncate.
        let err = reg
            .advance(
                ANYONE,
                &[IFsEpochRegistry::SignedEpoch {
                    epoch: 2,
                    entry: ep2.clone(),
                    sigPrev: sig2.clone(),
                }],
            )
            .unwrap_err();
        assert_eq!(revert_selector(err), IFsEpochRegistry::NotAdmin::SELECTOR);

        // Admin re-advances and clears adminNext.
        reg.advance(
            ADMIN,
            &[IFsEpochRegistry::SignedEpoch {
                epoch: 2,
                entry: ep2,
                sigPrev: sig2,
            }],
        )
        .unwrap();
        assert!(!reg.admin_next.read().unwrap());
        assert_eq!(reg.head.read().unwrap(), 2);
    });
}
