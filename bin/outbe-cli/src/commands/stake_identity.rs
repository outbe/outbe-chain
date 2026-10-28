//! Registration identity for `staking stake`.
//!
//! Callers pass either a `outbe-keygen validator` directory or the three
//! explicit registration fields. The directory path signs a
//! `OUTBE_VALIDATOR_REGISTRATION_V2` proof over `--validator`. The explicit
//! path forwards the supplied fields directly.

use std::path::Path;

use alloy_primitives::{Address, B256};
use commonware_codec::Encode as _;
use commonware_cryptography::Signer as _;
use eyre::Result;
use outbe_consensus::bls::{self, KeyBackend};
use outbe_primitives::validators::{validator_registration_message, VALIDATOR_REGISTRATION_DST};
use zeroize::Zeroizing;

use crate::errors::StakingError;
use crate::rpc::Rpc;

pub(crate) struct StakeIdentity {
    pub(crate) consensus_pubkey: Vec<u8>,
    pub(crate) radicle_node_id: B256,
    pub(crate) bls_registration_signature: Vec<u8>,
}

pub(crate) struct StakeIdentityInput<'a> {
    pub(crate) keys_dir: Option<&'a Path>,
    pub(crate) key_backend: &'a KeyBackend,
    pub(crate) consensus_pubkey: Option<&'a str>,
    pub(crate) radicle_node_id: Option<B256>,
    pub(crate) bls_registration_signature: Option<&'a str>,
}

pub(crate) async fn resolve_stake_identity(
    client: &(impl Rpc + Sync),
    validator: Address,
    input: StakeIdentityInput<'_>,
) -> Result<StakeIdentity> {
    let explicit_count = usize::from(input.consensus_pubkey.is_some())
        + usize::from(input.radicle_node_id.is_some())
        + usize::from(input.bls_registration_signature.is_some());
    if let Some(dir) = input.keys_dir {
        if explicit_count > 0 {
            return Err(StakingError::IdentitySourceConflict.into());
        }
        return load_keys_dir(client, validator, dir, input.key_backend).await;
    }
    let (Some(pubkey), Some(node_id), Some(signature)) = (
        input.consensus_pubkey,
        input.radicle_node_id,
        input.bls_registration_signature,
    ) else {
        return Err(StakingError::IdentityFlagsIncomplete.into());
    };
    Ok(StakeIdentity {
        consensus_pubkey: decode_hex_flag("--consensus-pubkey", pubkey)?,
        radicle_node_id: node_id,
        bls_registration_signature: decode_hex_flag("--bls-registration-signature", signature)?,
    })
}

async fn load_keys_dir(
    client: &(impl Rpc + Sync),
    validator: Address,
    keys_dir: &Path,
    key_backend: &KeyBackend,
) -> Result<StakeIdentity> {
    let evm_path = keys_dir.join("evm-key.hex");
    let evm_hex = Zeroizing::new(std::fs::read_to_string(&evm_path).map_err(|source| {
        StakingError::EvmKeyRead {
            path: evm_path.clone(),
            source,
        }
    })?);
    let derived =
        crate::tx::TxSigner::new(evm_hex.trim()).map_err(|source| StakingError::InvalidEvmKey {
            path: evm_path.clone(),
            source,
        })?;
    if derived.address() != validator {
        return Err(StakingError::ValidatorKeyMismatch {
            validator,
            derived: derived.address(),
            path: evm_path,
        }
        .into());
    }

    let radicle_path = keys_dir.join("radicle/keys");
    let keystore = radicle_crypto::ssh::Keystore::new(&radicle_path);
    let radicle_node_id = keystore
        .public_key()
        .map_err(|source| StakingError::RadicleNodeId {
            path: radicle_path.clone(),
            source,
        })?
        .map(|key| B256::from(key.into_inner()))
        .ok_or(StakingError::RadicleNodeIdMissing { path: radicle_path })?;

    let consensus_path = keys_dir.join("signing-key.hex");
    let consensus_key =
        bls::load_individual_key(&consensus_path, key_backend).map_err(|source| {
            StakingError::ConsensusKeyLoad {
                path: consensus_path,
                source,
            }
        })?;
    let encoded_pubkey = consensus_key.public_key().encode();
    let consensus_pubkey = encoded_pubkey.as_ref().to_vec();
    if consensus_pubkey.len() != 48 {
        return Err(StakingError::ConsensusPubkeyLength {
            actual: consensus_pubkey.len(),
        }
        .into());
    }

    let chain_id = client.eth_chain_id().await?;
    let bls_registration_signature =
        sign_registration(&consensus_key, chain_id, validator, radicle_node_id)?;
    Ok(StakeIdentity {
        consensus_pubkey,
        radicle_node_id,
        bls_registration_signature,
    })
}

fn sign_registration(
    key: &commonware_cryptography::bls12381::PrivateKey,
    chain_id: u64,
    validator: Address,
    radicle_node_id: B256,
) -> Result<Vec<u8>, StakingError> {
    let encoded = key.encode();
    let secret = Zeroizing::new(encoded.as_ref().to_vec());
    let blst_key = blst::min_pk::SecretKey::from_bytes(secret.as_slice())
        .map_err(StakingError::BlstSecretKey)?;
    let message = validator_registration_message(chain_id, validator, radicle_node_id);
    Ok(blst_key
        .sign(&message, VALIDATOR_REGISTRATION_DST, &[])
        .to_bytes()
        .to_vec())
}

fn decode_hex_flag(flag: &'static str, value: &str) -> Result<Vec<u8>, StakingError> {
    let stripped = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(stripped).map_err(|source| StakingError::InvalidHexFlag { flag, source })
}
