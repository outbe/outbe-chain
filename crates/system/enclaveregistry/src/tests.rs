use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::{SolCall, SolError};
use ed25519_dalek::{Signer as _, SigningKey as Ed25519SigningKey};
use k256::ecdsa::SigningKey as K256SigningKey;
use outbe_attestersregistry::AttestersRegistryContract;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::ATTESTERS_REGISTRY_ADDRESS;
use sha2::{Digest as _, Sha256};

use crate::precompile::{dispatch, IEnclaveRegistry};
use crate::runtime::{tcb_at_least, verify_ed25519_strict};
use crate::schema::EnclaveRegistryContract;
use crate::snp::{verify_snp_report, PRODUCT_GENOA, PRODUCT_MILAN, PRODUCT_TURIN};

const ADMIN: Address = Address::repeat_byte(0xAA);
const ATTESTER: Address = Address::repeat_byte(0x11);

fn revert_selector(err: PrecompileError) -> [u8; 4] {
    match err {
        PrecompileError::RevertBytes(bytes) if bytes.len() >= 4 => {
            bytes[..4].try_into().expect("selector")
        }
        other => panic!("expected typed revert, got {other:?}"),
    }
}

fn provider_at_block(block: u64) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(block);
    provider
}

fn build_manifest(provider: &str, profile: &str, base_url: &str, env: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&6u16.to_le_bytes());
    for field in [provider, profile, base_url, env] {
        out.extend_from_slice(&(field.len() as u32).to_le_bytes());
        out.extend_from_slice(field.as_bytes());
    }
    out
}

fn build_registration_v2(
    attester_sk: &K256SigningKey,
    enclave_sk: &Ed25519SigningKey,
    rm_digest: B256,
    measurements: &[[u8; 48]],
) -> (Address, Vec<u8>) {
    let attester_vk = attester_sk.verifying_key();
    let uncompressed = attester_vk.to_encoded_point(false);
    let attester = Address::from_slice(&keccak256(&uncompressed.as_bytes()[1..])[12..]);
    let compressed = attester_vk.to_encoded_point(true);

    let enclave_pk = enclave_sk.verifying_key().to_bytes();

    let mut payload = Vec::new();
    payload.push(2u8); // version 2
    payload.extend_from_slice(&enclave_pk);
    payload.extend_from_slice(rm_digest.as_slice());
    payload.extend_from_slice(&(measurements.len() as u32).to_le_bytes());
    for m in measurements {
        payload.extend_from_slice(&48u32.to_le_bytes());
        payload.extend_from_slice(m);
    }
    payload.extend_from_slice(&0u32.to_le_bytes()); // 0 platform TCB components
    payload.extend_from_slice(compressed.as_bytes());

    let mut binding_msg = Vec::new();
    binding_msg.extend_from_slice(b"bdp-enclave-v1");
    binding_msg.push(0x15);
    binding_msg.extend_from_slice(attester.as_slice());
    let enclave_sig = enclave_sk.sign(&binding_msg);
    payload.extend_from_slice(&enclave_sig.to_bytes());

    let mut reg_msg = Vec::new();
    reg_msg.extend_from_slice(b"bdp-enclave-v1");
    reg_msg.push(0x21);
    reg_msg.extend_from_slice(&payload);
    let prehash = keccak256(&reg_msg);
    let (sig, recid) = attester_sk
        .sign_prehash_recoverable(prehash.as_slice())
        .expect("sign");
    let sig = sig.normalize_s().unwrap_or(sig);
    let mut full = payload;
    full.extend_from_slice(&sig.to_bytes());
    full.push(recid.to_byte() + 27);
    (attester, full)
}

#[test]
fn tcb_bytewise_comparison() {
    assert!(tcb_at_least(0x7308_0000_0000_0003, 0x7308_0000_0000_0003));
    assert!(tcb_at_least(0x7408_0000_0000_0004, 0x7308_0000_0000_0003));
    assert!(!tcb_at_least(0x7407_0000_0000_0004, 0x7308_0000_0000_0003));
}

#[test]
fn ed25519_strict_verify_works() {
    let sk = Ed25519SigningKey::from_bytes(&[42u8; 32]);
    let pk = B256::from(sk.verifying_key().to_bytes());
    let sig = sk.sign(b"test-message");
    assert!(verify_ed25519_strict(pk, &sig.to_bytes(), b"test-message"));
    assert!(!verify_ed25519_strict(pk, &sig.to_bytes(), b"wrong"));
}

#[test]
fn snp_verifier_rejects_malformed_and_unknown_product() {
    assert!(verify_snp_report(&[0u8; 100], &[], PRODUCT_MILAN).is_err());
    assert!(verify_snp_report(&[0u8; 1184], &[], 0).is_err());
    assert!(verify_snp_report(&[0u8; 1184], &[], 4).is_err());
    assert!(verify_snp_report(&[0u8; 1184], &[], PRODUCT_GENOA).is_err());
    assert!(verify_snp_report(&[0u8; 1184], &[], PRODUCT_TURIN).is_err());
}

#[test]
fn genuine_milan_snp_report_verifies_against_pinned_amd_roots() {
    let report = hex::decode(include_str!("../tests/fixtures/amd/report_milan.hex").trim())
        .expect("fixture hex");
    let vcek = include_bytes!("../tests/fixtures/amd/vcek_milan.der");

    let fields = verify_snp_report(&report, vcek, PRODUCT_MILAN).expect("genuine Milan report");
    assert_eq!(fields.version, 2);
    assert_eq!(fields.vmpl, 0);
    assert_eq!(fields.policy, 0x30000);
    assert_eq!(fields.reported_tcb, 0x7308_0000_0000_0003);
    assert_eq!(fields.product.id(), PRODUCT_MILAN);
    assert_eq!(
        hex::encode(fields.measurement),
        "7a1e5c266c0108dbc9bb94fa926951320940915d0aafb42464bd88b579ea158d3e1a0dc39b2c60bd95b9c480cd81841f"
    );

    // Wrong product (Genoa) refuses a Milan report.
    assert!(verify_snp_report(&report, vcek, PRODUCT_GENOA).is_err());

    // Tampered signature byte refuses the report.
    let mut bad_sig = report.clone();
    bad_sig[0x2a0] ^= 0x01;
    assert!(verify_snp_report(&bad_sig, vcek, PRODUCT_MILAN).is_err());
}

#[test]
fn admin_register_live_checks_retire_and_revoke() {
    let mut provider = provider_at_block(100);
    StorageHandle::enter(&mut provider, |storage| {
        let mut attesters = AttestersRegistryContract::new(storage.clone());
        attesters.bootstrap_admin(ADMIN).unwrap();
        attesters
            .register(ADMIN, ATTESTER, 1 << 0, false, B256::ZERO, U256::ZERO)
            .unwrap();

        let mut reg = EnclaveRegistryContract::new(storage.clone());
        reg.bootstrap(ADMIN, ATTESTERS_REGISTRY_ADDRESS, true)
            .unwrap();

        let enclave_pk = B256::repeat_byte(0x77);
        let rm_digest = B256::repeat_byte(0x55);
        let provider_id = keccak256("plaid");
        let base_url_hash = keccak256("https://production.plaid.com");
        let mh = B256::repeat_byte(0x33);

        reg.set_measurement(ADMIN, mh, true).unwrap();
        reg.set_provider(
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

        dispatch(
            storage.clone(),
            &IEnclaveRegistry::adminRegisterCall {
                enclavePk: enclave_pk,
                attester: ATTESTER,
                rmDigest: rm_digest,
                providerId: provider_id,
                baseUrlHash: base_url_hash,
                measurementHash: mh,
            }
            .abi_encode(),
            ADMIN,
            U256::ZERO,
        )
        .unwrap();

        assert!(reg.is_active(enclave_pk).unwrap());
        assert!(reg
            .is_active_for(enclave_pk, ATTESTER, provider_id)
            .unwrap());
        assert!(!reg
            .is_active_for(enclave_pk, Address::repeat_byte(0x99), provider_id)
            .unwrap());

        // Retiring the measurement deactivates live check without mutating record.
        reg.set_measurement(ADMIN, mh, false).unwrap();
        assert!(!reg.is_active(enclave_pk).unwrap());
        reg.set_measurement(ADMIN, mh, true).unwrap();
        assert!(reg.is_active(enclave_pk).unwrap());

        // Demoting attester permission bit deactivates live check.
        attesters.update_mask(ADMIN, ATTESTER, 1 << 3).unwrap();
        assert!(!reg.is_active(enclave_pk).unwrap());
        attesters.update_mask(ADMIN, ATTESTER, 1 << 0).unwrap();
        assert!(reg.is_active(enclave_pk).unwrap());

        // Retire enters grace window (100 + 7200 = 7300).
        reg.retire(ATTESTER, enclave_pk).unwrap();
        assert_eq!(
            reg.get_deployment(enclave_pk).unwrap().status,
            IEnclaveRegistry::DeploymentStatus::Retiring
        );
        assert!(reg.is_active(enclave_pk).unwrap());

        // Revoke clears deployment and frees the key.
        reg.revoke(ADMIN, enclave_pk).unwrap();
        assert_eq!(
            reg.get_deployment(enclave_pk).unwrap().status,
            IEnclaveRegistry::DeploymentStatus::None
        );
        assert!(reg.measurements_of(enclave_pk).unwrap().is_empty());
        assert_eq!(reg.enclave_count().unwrap(), U256::from(1u64));
    });
}

#[test]
fn register_checks_attester_and_reaches_hardware_step_in_order() {
    let mut provider = provider_at_block(10);
    StorageHandle::enter(&mut provider, |storage| {
        let attester_sk = K256SigningKey::from_slice(&[7u8; 32]).unwrap();
        let enclave_sk = Ed25519SigningKey::from_bytes(&[9u8; 32]);
        let manifest = build_manifest("plaid", "release", "https://production.plaid.com", "prod");
        let rm_digest = B256::from_slice(&Sha256::digest(&manifest));
        let measurement = [0xABu8; 48];
        let (attester, registration) =
            build_registration_v2(&attester_sk, &enclave_sk, rm_digest, &[measurement]);

        let mut attesters = AttestersRegistryContract::new(storage.clone());
        attesters.bootstrap_admin(ADMIN).unwrap();
        attesters
            .register(ADMIN, attester, 1 << 0, false, B256::ZERO, U256::ZERO)
            .unwrap();

        let mut reg = EnclaveRegistryContract::new(storage.clone());
        reg.bootstrap(ADMIN, ATTESTERS_REGISTRY_ADDRESS, false)
            .unwrap();

        // Wrong caller triggers NotTheAttester before hardware verification.
        let err = reg
            .register(
                Address::repeat_byte(0xFE),
                &registration,
                &[0u8; 1184],
                &[0u8; 64],
                PRODUCT_MILAN,
                &manifest,
            )
            .unwrap_err();
        assert_eq!(
            revert_selector(err),
            IEnclaveRegistry::NotTheAttester::SELECTOR
        );

        // Right caller passes Check 1 (attester + Ed25519 binding) and Check 2,
        // failing at Check 3 with ReportRefused on dummy report bytes.
        let err = reg
            .register(
                attester,
                &registration,
                &[0u8; 1184],
                &[0u8; 64],
                PRODUCT_MILAN,
                &manifest,
            )
            .unwrap_err();
        assert_eq!(
            revert_selector(err),
            IEnclaveRegistry::ReportRefused::SELECTOR
        );
    });
}
