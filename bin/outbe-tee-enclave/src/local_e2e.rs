//! Explicit software SGX simulation for local process E2E tests only.
//! No hardware identity, isolation, or confidentiality is claimed. Keys are
//! derived from public fixtures. LE2E blobs are never production SGX blobs.
use crate::{
    initialization::InitializationState,
    keys::EnclaveKeys,
    seal::{self, EnclaveBootConfig, KeyPolicy, SealHeader, UnsealedTributeOfferAndGroupSig},
    transport,
};
use alloy_primitives::{keccak256, U256};
use outbe_primitives::{
    chain::DEVNET_CHAIN_ID,
    tee_attestation_v1::{AttestationMode, NetworkBindingV1, TrustedNetworkDescriptorV1},
};
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

struct Fixture {
    measurement: [u8; 32],
    signer: [u8; 32],
    platform: [u8; 32],
    combined: bool,
}
static FIXTURE: OnceLock<Fixture> = OnceLock::new();
pub(crate) fn configured() -> bool {
    FIXTURE.get().is_some()
}
fn key(combined: bool) -> [u8; 32] {
    let f = FIXTURE.get().expect("explicit local E2E fixture");
    fixture_key(f, combined)
}
fn fixture_key(f: &Fixture, combined: bool) -> [u8; 32] {
    let mut bytes = b"PUBLIC LOCAL E2E SOFTWARE SEAL / NOT SGX".to_vec();
    bytes.extend_from_slice(&f.signer);
    bytes.extend_from_slice(&f.platform);
    if combined {
        bytes.extend_from_slice(&f.measurement);
    }
    keccak256(bytes).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulated_policies_distinguish_identity_and_platform() {
        let fixture = |measurement, signer, platform| Fixture {
            measurement: [measurement; 32],
            signer: [signer; 32],
            platform: [platform; 32],
            combined: true,
        };
        let original = fixture(1, 2, 3);
        let changed_measurement = fixture(4, 2, 3);
        assert_eq!(
            fixture_key(&original, false),
            fixture_key(&changed_measurement, false)
        );
        assert_ne!(
            fixture_key(&original, true),
            fixture_key(&changed_measurement, true)
        );
        for changed in [fixture(1, 4, 3), fixture(1, 2, 4)] {
            for combined in [false, true] {
                assert_ne!(
                    fixture_key(&original, combined),
                    fixture_key(&changed, combined)
                );
            }
        }
    }

    #[test]
    fn software_backend_rejects_non_local_networks_and_production_blobs() {
        let binding = NetworkBindingV1 {
            chain_id: U256::from(DEVNET_CHAIN_ID).to_be_bytes::<32>(),
            genesis_hash: alloy_primitives::B256::ZERO,
            attestation_mode: AttestationMode::GramineDirectDev,
        };
        assert!(validate_binding(binding).is_ok());
        assert!(validate_binding(NetworkBindingV1 {
            chain_id: [1; 32],
            ..binding
        })
        .is_err());
        assert!(validate_binding(NetworkBindingV1 {
            attestation_mode: AttestationMode::DcapRequired,
            ..binding
        })
        .is_err());
        for bytes in [b"TSGX1".as_slice(), b"TSEAL", b""] {
            assert!(unseal(bytes, 0).is_err());
        }
    }
}
fn validate_binding(binding: NetworkBindingV1) -> Result<(), String> {
    if binding.chain_id != U256::from(DEVNET_CHAIN_ID).to_be_bytes::<32>()
        || binding.attestation_mode != AttestationMode::GramineDirectDev
    {
        return Err("local E2E fixture accepts only DirectDev devnet".into());
    }
    Ok(())
}
pub(crate) fn seal(
    secret: &[u8; 32],
    extra: &[u8],
    binding: NetworkBindingV1,
    header: &SealHeader,
) -> Result<Vec<u8>, String> {
    validate_binding(binding)?;
    let combined = FIXTURE.get().unwrap().combined;
    let mut header = *header;
    header.key_policy = KeyPolicy::Mock;
    let mut bytes = if combined {
        b"LE2E1".to_vec()
    } else {
        b"LE2E0".to_vec()
    };
    bytes.extend(
        seal::seal_tribute_offer_and_group_sig(secret, extra, &key(combined), binding, &header)
            .map_err(|e| e.to_string())?,
    );
    Ok(bytes)
}
pub(crate) fn unseal(bytes: &[u8], svn: u16) -> Result<UnsealedTributeOfferAndGroupSig, String> {
    let combined = if bytes.starts_with(b"LE2E1") {
        true
    } else if bytes.starts_with(b"LE2E0") {
        false
    } else {
        return Err("not an explicitly simulated local seal".into());
    };
    let value = seal::unseal_network_bound_payload(&bytes[5..], &key(combined), svn)
        .map_err(|e| e.to_string())?;
    validate_binding(value.network_binding)?;
    if value.header.key_policy != KeyPolicy::Mock {
        return Err("invalid local seal policy".into());
    }
    Ok(value)
}

pub fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| -> Result<String, String> {
        args.windows(2)
            .find(|a| a[0] == name)
            .map(|a| a[1].clone())
            .ok_or_else(|| format!("missing {name}"))
    };
    let hex32 = |name: &str| -> Result<[u8; 32], String> {
        hex::decode(arg(name)?.trim_start_matches("0x"))
            .map_err(|e| e.to_string())?
            .try_into()
            .map_err(|_| format!("invalid {name}"))
    };
    let descriptor = TrustedNetworkDescriptorV1::decode_canonical(
        &std::fs::read(arg("--network-descriptor")?).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    validate_binding(descriptor.network_binding)?;
    outbe_consensus::config::init_consensus_chain_id(DEVNET_CHAIN_ID).map_err(|e| e.to_string())?;
    let policy = arg("--seal-policy")?;
    if policy != "legacy" && policy != "combined" {
        return Err("seal policy must be legacy or combined".into());
    }
    FIXTURE
        .set(Fixture {
            measurement: hex32("--measurement")?,
            signer: hex32("--signer")?,
            platform: hex32("--platform")?,
            combined: policy == "combined",
        })
        .map_err(|_| "fixture already configured")?;
    let dir = PathBuf::from(arg("--tee-dir")?);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let boot = Arc::new(EnclaveBootConfig::new(
        descriptor.network_binding.chain_id,
        dir,
        0,
    ));
    let keys = Arc::new(EnclaveKeys::new([0; 32], Some(hex32("--identity-seed")?))?);
    let state = Arc::new(InitializationState::local_e2e(
        boot.clone(),
        &keys,
        descriptor.clone(),
    )?);
    let offer_key: transport::SharedTributeOfferKey = Arc::new(OnceLock::new());
    if boot.sealed_root_path().exists() && state.manifest()?.is_none() {
        return Err("seal exists without initialization authority".into());
    }
    if let Some(key) =
        transport::unseal_tribute_offer_and_group_sig_on_boot(&boot, descriptor.network_binding)?
    {
        offer_key.set(key).map_err(|_| "key already loaded")?;
    }
    let endpoint: std::net::SocketAddr = arg("--socket")?
        .parse()
        .map_err(|e| format!("endpoint: {e}"))?;
    if !endpoint.ip().is_loopback() {
        return Err("local E2E requires loopback".into());
    }
    let listener = std::net::TcpListener::bind(endpoint).map_err(|e| e.to_string())?;
    eprintln!("LOCAL_E2E: software identity/sealing, no SGX guarantees; listening {endpoint}");
    transport::serve_tcp(
        &listener,
        keys,
        Some(boot),
        offer_key,
        state,
        descriptor.network_binding.chain_id.into(),
    )
    .map_err(|e| e.to_string())
}
