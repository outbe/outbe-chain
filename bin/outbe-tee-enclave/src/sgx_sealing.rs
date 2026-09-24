//! SGX sealing bound to both code and signer, without changing Gramine.
//!
//! TSGX1 || canonical KEYREQUEST (512 bytes) || existing TSEAL payload.
//! The complete request is HKDF-bound to the AEAD key. Restarts use its original
//! CPU/ISV/CONFIG SVN values, so a platform TCB update does not lose sealed state.
//! No raw key ever crosses the enclave interface.
use crate::seal::{self, KeyPolicy, SealHeader, UnsealedTributeOfferAndGroupSig};
use outbe_primitives::tee_attestation_v1::NetworkBindingV1;
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8; 5] = b"TSGX1";
const REQUEST_LEN: usize = 512;
const KEY_ID: &[u8] = b"outbe/tee/seal/both/v1";
const FLAGS_MASK: u64 = 0xffff_ffff_ffff_fff3;

#[repr(C, align(512))]
struct KeyRequest([u8; REQUEST_LEN]);
#[repr(C, align(16))]
struct HardwareKey([u8; 16]);

fn parameters(cpu_svn: [u8; 16], isv_svn: u16, config_svn: u16) -> KeyRequest {
    let mut r = KeyRequest([0; REQUEST_LEN]);
    r.0[0..2].copy_from_slice(&4_u16.to_le_bytes()); // SEAL_KEY
    r.0[2..4].copy_from_slice(&3_u16.to_le_bytes()); // MRENCLAVE | MRSIGNER
    r.0[4..6].copy_from_slice(&isv_svn.to_le_bytes());
    r.0[8..24].copy_from_slice(&cpu_svn);
    r.0[24..32].copy_from_slice(&FLAGS_MASK.to_le_bytes());
    r.0[40..40 + KEY_ID.len()].copy_from_slice(KEY_ID);
    r.0[72..76].copy_from_slice(&u32::MAX.to_le_bytes());
    r.0[76..78].copy_from_slice(&config_svn.to_le_bytes());
    r
}

fn decode_parameters(bytes: &[u8]) -> Result<KeyRequest, String> {
    if bytes.len() != REQUEST_LEN {
        return Err("invalid SGX key request length".into());
    }
    let cpu = bytes[8..24].try_into().map_err(|_| "invalid CPU SVN")?;
    let isv = u16::from_le_bytes([bytes[4], bytes[5]]);
    let config = u16::from_le_bytes([bytes[76], bytes[77]]);
    let expected = parameters(cpu, isv, config);
    if bytes != expected.0 {
        return Err("non-canonical combined SGX sealing request".into());
    }
    Ok(expected)
}

fn current_parameters(expected_svn: u16) -> Result<KeyRequest, String> {
    let report = std::fs::read("/dev/attestation/report")
        .map_err(|e| format!("read SGX sealing report: {e}"))?;
    if report.len() != 432 {
        return Err("invalid local SGX report length".into());
    }
    let isv = u16::from_le_bytes([report[258], report[259]]);
    if isv != expected_svn {
        return Err("sealing SVN differs from local SGX report".into());
    }
    Ok(parameters(
        report[..16]
            .try_into()
            .map_err(|_| "invalid local CPU SVN")?,
        isv,
        u16::from_le_bytes([report[260], report[261]]),
    ))
}

#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)] // EGETKEY has no safe Rust/Gramine API for the combined policy.
fn hardware_key(request: &KeyRequest) -> Result<Zeroizing<[u8; 32]>, String> {
    // This capability probe prevents executing ENCLU in normal host/unit-test
    // processes. Attestation mode "none" on real SGX remains supported.
    let probe = crate::gramine::sealing_key_raw(true)
        .map_err(|e| format!("SGX sealing is unavailable: {e}"))?;
    let mut probe = Zeroizing::new(probe);
    probe.zeroize();
    let mut raw = HardwareKey([0; 16]);
    let status: u64;
    // SAFETY: both operands are aligned, resident enclave memory; KEYREQUEST
    // is fully initialized and canonical. RBX is preserved for the SysV ABI.
    unsafe {
        core::arch::asm!(
            "mov r10, rbx",
            "mov rbx, rdi",
            "enclu",
            "mov rbx, r10",
            inout("rax") 1_u64 => status,
            in("rdi") request.0.as_ptr(),
            in("rcx") raw.0.as_mut_ptr(),
            lateout("r10") _,
            options(nostack),
        );
    }
    if status != 0 {
        raw.0.zeroize();
        return Err(format!("combined SGX EGETKEY failed: {status:#x}"));
    }
    let result = crate::crypto::hkdf_sha256(&request.0, &raw.0, b"outbe/tee/seal-key/both/v1")
        .map(Zeroizing::new)
        .map_err(|e| e.to_string());
    raw.0.zeroize();
    result
}

#[cfg(not(target_arch = "x86_64"))]
fn hardware_key(_: &KeyRequest) -> Result<Zeroizing<[u8; 32]>, String> {
    Err("combined SGX sealing requires x86_64".into())
}

pub(crate) fn seal_payload(
    secret: &[u8; 32],
    extra: &[u8],
    binding: NetworkBindingV1,
    header: &SealHeader,
) -> Result<Vec<u8>, String> {
    #[cfg(feature = "local-e2e")]
    if crate::local_e2e::configured() {
        return crate::local_e2e::seal(secret, extra, binding, header);
    }
    #[cfg(any(test, feature = "mock"))]
    if !crate::gramine::attestation_type().sgx_present() {
        let mut header = *header;
        header.key_policy = KeyPolicy::Mock;
        return seal::seal_tribute_offer_and_group_sig(
            secret,
            extra,
            &seal::MOCK_SEALING_KEY,
            binding,
            &header,
        )
        .map_err(|e| e.to_string());
    }
    let request = current_parameters(header.isv_svn)?;
    let key = hardware_key(&request)?;
    let mut header = *header;
    header.key_policy = KeyPolicy::MrEnclaveAndSigner;
    let payload = seal::seal_tribute_offer_and_group_sig(secret, extra, &key, binding, &header)
        .map_err(|e| e.to_string())?;
    let mut blob = Vec::with_capacity(MAGIC.len() + REQUEST_LEN + payload.len());
    blob.extend_from_slice(MAGIC);
    blob.extend_from_slice(&request.0);
    blob.extend_from_slice(&payload);
    Ok(blob)
}

pub(crate) fn unseal_payload(
    blob: &[u8],
    running_svn: u16,
) -> Result<UnsealedTributeOfferAndGroupSig, String> {
    #[cfg(feature = "local-e2e")]
    if crate::local_e2e::configured() {
        return crate::local_e2e::unseal(blob, running_svn);
    }
    if blob.starts_with(MAGIC) {
        if blob.len() < MAGIC.len() + REQUEST_LEN {
            return Err("truncated combined SGX seal".into());
        }
        let request = decode_parameters(&blob[MAGIC.len()..MAGIC.len() + REQUEST_LEN])?;
        let key = hardware_key(&request)?;
        let value = seal::unseal_network_bound_payload(
            &blob[MAGIC.len() + REQUEST_LEN..],
            &key,
            running_svn,
        )
        .map_err(|e| e.to_string())?;
        if value.header.key_policy != KeyPolicy::MrEnclaveAndSigner
            || value.header.isv_svn != u16::from_le_bytes([request.0[4], request.0[5]])
        {
            return Err("combined seal metadata mismatch".into());
        }
        return Ok(value);
    }
    // Read-only compatibility for old operator state. New production writes
    // always use combined sealing; corruption never falls back to fresh state.
    let (key, policy) =
        crate::transport::sealing_key().ok_or_else(|| "SGX sealing key unavailable".to_string())?;
    let key = Zeroizing::new(key);
    let value =
        seal::unseal_network_bound_payload(blob, &key, running_svn).map_err(|e| e.to_string())?;
    if value.header.key_policy != policy {
        return Err("legacy seal policy mismatch".into());
    }
    Ok(value)
}

pub(crate) fn unseal_bound(
    blob: &[u8],
    binding: NetworkBindingV1,
    svn: u16,
) -> Result<UnsealedTributeOfferAndGroupSig, String> {
    let value = unseal_payload(blob, svn)?;
    if value.network_binding != binding {
        return Err("sealed blob network mismatch".into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_layout_and_policy_are_fixed_and_versions_roundtrip() {
        assert_eq!(core::mem::size_of::<KeyRequest>(), 512);
        assert_eq!(core::mem::align_of::<KeyRequest>(), 512);
        assert_eq!(core::mem::align_of::<HardwareKey>(), 16);
        let req = parameters([7; 16], 12, 3);
        assert_eq!(decode_parameters(&req.0).unwrap().0, req.0);
        for offset in [0, 2, 6, 24, 32, 40, 72, 78, 511] {
            let mut changed = req.0;
            changed[offset] ^= 1;
            assert!(decode_parameters(&changed).is_err(), "offset {offset}");
        }
    }
    #[test]
    fn combined_envelope_never_falls_back_to_mock_or_legacy() {
        assert!(unseal_payload(b"TSGX1", 1).is_err());
        let mut blob = MAGIC.to_vec();
        blob.extend_from_slice(&parameters([0; 16], 1, 0).0);
        blob.extend_from_slice(b"TSEAL");
        assert!(unseal_payload(&blob, 1).is_err());
    }
}

/// Hardware-only probe of the exact production implementation, using public test data.
#[cfg(feature = "sgx-sealing-probe")]
pub fn hardware_probe(action: &str, path: &std::path::Path) -> Result<(), String> {
    use alloy_primitives::{B256, U256};
    use outbe_primitives::tee_attestation_v1::AttestationMode;
    let binding = NetworkBindingV1 {
        chain_id: U256::from(54322345_u64).to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x71),
        attestation_mode: AttestationMode::GramineDirectDev,
    };
    let report = std::fs::read("/dev/attestation/report").map_err(|e| e.to_string())?;
    if report.len() != 432 {
        return Err("invalid hardware report".into());
    }
    let svn = u16::from_le_bytes([report[258], report[259]]);
    match action {
        "seal" | "legacy-seal" => {
            let mut header = SealHeader {
                format_version: seal::SEAL_FORMAT,
                key_policy: KeyPolicy::MrEnclaveAndSigner,
                isv_svn: svn,
                key_epoch: 0,
                tribute_offer_epoch: 0,
                nonce: [0x33; 12],
            };
            let blob = if action == "legacy-seal" {
                let (key, policy) = crate::transport::sealing_key()
                    .ok_or_else(|| "legacy SGX sealing key unavailable".to_string())?;
                let key = Zeroizing::new(key);
                if policy != KeyPolicy::MrSigner {
                    return Err("legacy probe requires hardware MRSIGNER sealing".into());
                }
                header.key_policy = policy;
                seal::seal_tribute_offer_and_group_sig(
                    &[0x55; 32],
                    b"public hardware test data",
                    &key,
                    binding,
                    &header,
                )
                .map_err(|error| error.to_string())?
            } else {
                seal_payload(&[0x55; 32], b"public hardware test data", binding, &header)?
            };
            if action == "seal" && !blob.starts_with(MAGIC) {
                return Err("probe did not use hardware sealing".into());
            }
            std::fs::write(path, blob).map_err(|e| e.to_string())?;
        }
        "unseal" | "reseal" => {
            let blob = std::fs::read(path).map_err(|e| e.to_string())?;
            let value = unseal_bound(&blob, binding, svn)?;
            if *value.tribute_offer_secret != [0x55; 32]
                || value.group_sig.as_slice() != b"public hardware test data"
            {
                return Err("probe payload differs".into());
            }
            if action == "reseal" {
                if !blob.starts_with(seal::SEAL_MAGIC)
                    || value.header.key_policy != KeyPolicy::MrSigner
                {
                    return Err("reseal probe requires a legacy MRSIGNER payload".into());
                }
                let mut header = value.header;
                header.isv_svn = svn;
                header.nonce = [0x44; 12];
                let migrated = seal_payload(
                    &value.tribute_offer_secret,
                    value.group_sig.as_slice(),
                    binding,
                    &header,
                )?;
                if !migrated.starts_with(MAGIC) {
                    return Err("legacy migration did not produce combined hardware sealing".into());
                }
                std::fs::write(path, migrated).map_err(|error| error.to_string())?;
            }
        }
        _ => return Err("expected seal, legacy-seal, unseal or reseal".into()),
    }
    Ok(())
}
