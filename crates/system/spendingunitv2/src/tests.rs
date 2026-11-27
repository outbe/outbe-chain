use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolError};
use blst::min_sig::SecretKey as BlsSecretKey;
use ed25519_dalek::{Signer as _, SigningKey as Ed25519SigningKey};
use outbe_attestersregistry::AttestersRegistryContract;
use outbe_enclaveregistry::{EnclaveRegistryContract, IEnclaveRegistry};
use outbe_fsepochregistry::{
    digest_of, encode_eip2537_g1, encode_eip2537_g2, FsEpochRegistryContract, IFsEpochRegistry,
};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::{
    ATTESTERS_REGISTRY_ADDRESS, ENCLAVE_REGISTRY_ADDRESS, FS_EPOCH_REGISTRY_ADDRESS,
};

use crate::precompile::{dispatch, ISpendingUnitV2};
use crate::runtime::{seal_id, settlement_id, settlement_message};
use crate::schema::SpendingUnitV2Contract;
use crate::worldwide_day::{day_number, today_utc_plus_14};

const ADMIN: Address = Address::repeat_byte(0xAA);
const ATTESTER: Address = Address::repeat_byte(0x11);
const REFERRER: Address = Address::repeat_byte(0x22);

fn revert_selector(err: PrecompileError) -> [u8; 4] {
    match err {
        PrecompileError::RevertBytes(bytes) if bytes.len() >= 4 => {
            bytes[..4].try_into().expect("selector")
        }
        other => panic!("expected typed revert, got {other:?}"),
    }
}

fn bls_keypair(seed_byte: u8) -> (BlsSecretKey, Bytes) {
    let sk = BlsSecretKey::key_gen(&[seed_byte; 32], &[]).expect("key_gen");
    let pk = sk.sk_to_pk();
    (sk, Bytes::from(encode_eip2537_g2(&pk).to_vec()))
}

fn bls_sign(sk: &BlsSecretKey, msg: &[u8], dst: &[u8]) -> Bytes {
    let sig = sk.sign(msg, dst, &[]);
    Bytes::from(encode_eip2537_g1(&sig).to_vec())
}

fn handle_message(
    attester: Address,
    ctx: B256,
    sid: B256,
    count: u64,
    f_fold: B256,
    pool_epoch: u32,
) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(b"bdp-fs-handle-v4");
    msg.extend_from_slice(attester.as_slice());
    msg.extend_from_slice(ctx.as_slice());
    msg.extend_from_slice(&32u32.to_le_bytes());
    msg.extend_from_slice(sid.as_slice());
    msg.extend_from_slice(&count.to_le_bytes());
    msg.extend_from_slice(f_fold.as_slice());
    msg.extend_from_slice(&pool_epoch.to_le_bytes());
    msg
}

/// Pins `seal_id`, `settlement_id`, and `settlement_message` against the cross-repo
/// `settlement_parity.json` fixture shared with `bank-data-provider`.
#[test]
fn settlement_derivations_match_bdp_parity_fixture() {
    let ctx = B256::repeat_byte(0xc7);
    let rm_digest = B256::repeat_byte(0x5a);
    let commitment_root = Bytes::from(vec![0x11u8; 48]);

    let fold0 = B256::from_slice(
        &hex::decode("30644e72e131a029b85045b68181585d2833e84879b9709143e1f593effffffc").unwrap(),
    );
    let fold1 = B256::from_slice(
        &hex::decode("0a8b4bafe0e957560923ce81b942e03477423198cba97a1d03a498681c5073fa").unwrap(),
    );
    let total_fold = B256::from_slice(
        &hex::decode("0a8b4bafe0e957560923ce81b942e03477423198cba97a1d03a498681c5073f5").unwrap(),
    );

    assert_eq!(
        seal_id(ctx, 9, 5, 40, fold0),
        B256::from_slice(
            &hex::decode("b2212da341106c52f5a7a52e504c81bdf8312b885f48cd5be0fbc76a94cc005b")
                .unwrap()
        )
    );
    assert_eq!(
        seal_id(ctx, 9, 6, 2, fold1),
        B256::from_slice(
            &hex::decode("a412bdcde57fb1651c2e2b5daedec27249a109ee13d07296fca17acd51d00610")
                .unwrap()
        )
    );
    assert_eq!(
        settlement_id(ctx, 9),
        B256::from_slice(
            &hex::decode("b0b9374c5ff30da9c7d4c646e31432e7478b8733cf6b28825abc3915b8a5fb0a")
                .unwrap()
        )
    );

    let mint = ISpendingUnitV2::Mint {
        suId: U256::from(1u64),
        derivedOwner: B256::repeat_byte(0x01),
        referrerAddress: REFERRER,
        enclavePk: B256::repeat_byte(0x02),
        providerId: B256::repeat_byte(0x03),
        currency: 978,
        amountBase: 1234,
        amountAtto: 250_000_000_000_000,
    };
    let batch = ISpendingUnitV2::Batch {
        ctx,
        sequence: 9,
        commitmentRoot: commitment_root,
        worldwideDay: 20_260_924,
        count: 42,
        fFold: total_fold,
        settleSig: Bytes::from(vec![0u8; 64]),
    };
    let msg = settlement_message(&mint, &batch, rm_digest).unwrap();
    let expected_msg = hex::decode(
        "6264702d656e636c6176652d763112c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c75a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a300000001111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111110900000000000000d203d20400000000000000a031a95fe300003c283501000000002a000000000000000a8b4bafe0e957560923ce81b942e03477423198cba97a1d03a498681c5073f5",
    )
    .unwrap();
    assert_eq!(msg, expected_msg);
}

#[test]
fn worldwide_day_matches_shared_vectors() {
    assert_eq!(day_number(19_700_101), Some(0));
    assert_eq!(day_number(20_240_229), Some(19_782));
    assert_eq!(day_number(20_260_924), Some(20_720));
    assert_eq!(day_number(20_230_229), None);
    assert_eq!(today_utc_plus_14(1_790_243_999), 20_720);
    assert_eq!(today_utc_plus_14(1_790_244_000), 20_721);
}

#[test]
fn end_to_end_two_pool_settlement_and_multicall_atomicity() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(50);
    // 2026-09-24 at UTC+14 (day number 20720).
    provider.set_timestamp(U256::from(1_790_243_999u64));

    StorageHandle::enter(&mut provider, |storage| {
        // 1. Setup AttestersRegistry.
        let mut attesters = AttestersRegistryContract::new(storage.clone());
        attesters.bootstrap_admin(ADMIN).unwrap();
        attesters
            .register(ADMIN, ATTESTER, 1 << 0, false, B256::ZERO, U256::ZERO)
            .unwrap();

        // 2. Setup EnclaveRegistry with devnet adminRegister.
        let enclave_sk = Ed25519SigningKey::from_bytes(&[33u8; 32]);
        let enclave_pk = B256::from(enclave_sk.verifying_key().to_bytes());
        let rm_digest = B256::repeat_byte(0x5a);
        let provider_id = keccak256("plaid");
        let base_url_hash = keccak256("https://production.plaid.com");
        let mh = B256::repeat_byte(0x44);

        let mut enclaves = EnclaveRegistryContract::new(storage.clone());
        enclaves
            .bootstrap(ADMIN, ATTESTERS_REGISTRY_ADDRESS, true)
            .unwrap();
        enclaves.set_measurement(ADMIN, mh, true).unwrap();
        enclaves
            .set_provider(
                ADMIN,
                provider_id,
                &IEnclaveRegistry::ProviderPolicy {
                    allowed: true,
                    requireRelease: true,
                    baseUrlHash: base_url_hash,
                    environmentHash: B256::ZERO,
                },
            )
            .unwrap();
        enclaves
            .admin_register(
                ADMIN,
                enclave_pk,
                ATTESTER,
                rm_digest,
                provider_id,
                base_url_hash,
                mh,
            )
            .unwrap();

        // 3. Setup FsEpochRegistry with epochs 0 (2026-09-14) and 1 (2026-09-21).
        let (sk0, pk0) = bls_keypair(20);
        let dst0 = Bytes::from_static(b"PSO-FS-ATT-E0");
        let ep0 = IFsEpochRegistry::Epoch {
            pkAtt: pk0.clone(),
            dst: dst0.clone(),
            startWwd: 20_260_914,
            membersHash: B256::repeat_byte(0x01),
            prevHash: B256::ZERO,
        };
        let d0 = digest_of(0, &pk0, &dst0, ep0.startWwd, ep0.membersHash, ep0.prevHash).unwrap();

        let (sk1, pk1) = bls_keypair(21);
        let dst1 = Bytes::from_static(b"PSO-FS-ATT-E1");
        let ep1 = IFsEpochRegistry::Epoch {
            pkAtt: pk1.clone(),
            dst: dst1.clone(),
            startWwd: 20_260_921,
            membersHash: B256::repeat_byte(0x02),
            prevHash: d0,
        };
        let d1 = digest_of(1, &pk1, &dst1, ep1.startWwd, ep1.membersHash, ep1.prevHash).unwrap();
        let sig1 = bls_sign(&sk0, d1.as_slice(), &dst0);

        let mut fs = FsEpochRegistryContract::new(storage.clone());
        fs.bootstrap_admin(ADMIN).unwrap();
        fs.anchor(ADMIN, &ep0).unwrap();
        fs.advance(
            ATTESTER,
            &[IFsEpochRegistry::SignedEpoch {
                epoch: 1,
                entry: ep1,
                sigPrev: sig1,
            }],
        )
        .unwrap();

        // 4. Setup SpendingUnitV2.
        let mut su = SpendingUnitV2Contract::new(storage.clone());
        su.bootstrap(
            ADMIN,
            ATTESTERS_REGISTRY_ADDRESS,
            ENCLAVE_REGISTRY_ADDRESS,
            FS_EPOCH_REGISTRY_ADDRESS,
        )
        .unwrap();

        // Build a two-pool settlement whose folds wrap modulo BN254 r.
        let ctx = B256::repeat_byte(0xc7);
        let seq = 9u64;
        let fold0 = B256::from_slice(
            &hex::decode("30644e72e131a029b85045b68181585d2833e84879b9709143e1f593effffffc")
                .unwrap(),
        );
        let fold1 = B256::from_slice(
            &hex::decode("0a8b4bafe0e957560923ce81b942e03477423198cba97a1d03a498681c5073fa")
                .unwrap(),
        );
        let total_fold = B256::from_slice(
            &hex::decode("0a8b4bafe0e957560923ce81b942e03477423198cba97a1d03a498681c5073f5")
                .unwrap(),
        );

        let sid0 = seal_id(ctx, seq, 0, 40, fold0);
        let sid1 = seal_id(ctx, seq, 1, 2, fold1);
        let hsig0 = bls_sign(
            &sk0,
            &handle_message(ATTESTER, ctx, sid0, 40, fold0, 0),
            &dst0,
        );
        let hsig1 = bls_sign(
            &sk1,
            &handle_message(ATTESTER, ctx, sid1, 2, fold1, 1),
            &dst1,
        );

        let mint = ISpendingUnitV2::Mint {
            suId: U256::from(1001u64),
            derivedOwner: B256::from(U256::from(999u64)),
            referrerAddress: REFERRER,
            enclavePk: enclave_pk,
            providerId: provider_id,
            currency: 978,
            amountBase: 1234,
            amountAtto: 250_000_000_000_000,
        };
        let mut batch = ISpendingUnitV2::Batch {
            ctx,
            sequence: seq,
            commitmentRoot: Bytes::from(vec![0x11u8; 48]),
            worldwideDay: 20_260_924,
            count: 42,
            fFold: total_fold,
            settleSig: Bytes::new(),
        };
        let msg = settlement_message(&mint, &batch, rm_digest).unwrap();
        batch.settleSig = Bytes::from(enclave_sk.sign(&msg).to_bytes().to_vec());

        let seals = vec![
            ISpendingUnitV2::Seal {
                poolEpoch: 0,
                count: 40,
                fFold: fold0,
                handleSig: hsig0,
            },
            ISpendingUnitV2::Seal {
                poolEpoch: 1,
                count: 2,
                fFold: fold1,
                handleSig: hsig1,
            },
        ];

        // Dispatch via ABI.
        let submit_call = ISpendingUnitV2::submitSUCall {
            mint: mint.clone(),
            batch: batch.clone(),
            seals: seals.clone(),
        };
        dispatch(
            storage.clone(),
            &submit_call.abi_encode(),
            ATTESTER,
            U256::ZERO,
        )
        .unwrap();

        assert!(su.committed.read(&U256::from(1001u64)).unwrap());
        assert_eq!(su.total_committed.read().unwrap(), U256::from(1u64));
        let expected_sid = settlement_id(ctx, seq);
        assert_eq!(su.settlements.read(&ctx).unwrap(), expected_sid);
        let entity = su.get_data(U256::from(1001u64)).unwrap();
        assert_eq!(entity.attesterAddress, ATTESTER);
        assert_eq!(entity.sealId, expected_sid);
        assert_eq!(entity.worldwideDay, 20_260_924);

        // Resubmitting same ctx with a new suId fails with AlreadySettled.
        let mut mint2 = mint.clone();
        mint2.suId = U256::from(1002u64);
        let err = su.submit_su(ATTESTER, &mint2, &batch, &seals).unwrap_err();
        assert_eq!(
            revert_selector(err),
            ISpendingUnitV2::AlreadySettled::SELECTOR
        );

        // Multicall rejects non-submitSU selectors.
        let err = su
            .multicall(
                ATTESTER,
                &[Bytes::from(ISpendingUnitV2::pauseCall {}.abi_encode())],
            )
            .unwrap_err();
        assert_eq!(
            revert_selector(err),
            ISpendingUnitV2::MulticallMethodNotAllowed::SELECTOR
        );
    });
}
