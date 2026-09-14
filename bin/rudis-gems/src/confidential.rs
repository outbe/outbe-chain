//! Promis wallet keys, mint/burn MACs and confidential balance decryption.
use crate::{rpc::Rpc, wallet::Wallet};
use alloy_primitives::{Address, Bytes, B256, U256};
use eyre::{ensure, Result};
use k256::ecdsa::signature::hazmat::PrehashSigner;
use outbe_tee::protocol::{derive_account_keys_message, eip191_hash, Ledger, PromisOp};
use ring::{aead, agreement, hkdf, hmac, rand::SystemRandom};
use serde::Deserialize;
use zeroize::Zeroizing;

pub struct Keys(Zeroizing<[u8; 64]>);
struct KeyLength;
impl hkdf::KeyType for KeyLength {
    fn len(&self) -> usize {
        32
    }
}

fn hkdf32(salt: &[u8], input: &[u8], info: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, salt);
    let prk = salt.extract(input);
    let infos = [info];
    let okm = prk
        .expand(&infos, KeyLength)
        .map_err(|_| eyre::eyre!("HKDF expand failed"))?;
    let mut out = Zeroizing::new([0; 32]);
    okm.fill(out.as_mut())
        .map_err(|_| eyre::eyre!("HKDF fill failed"))?;
    Ok(out)
}

fn decrypt(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let key = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|_| eyre::eyre!("Invalid encryption key"))?;
    let nonce = aead::Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| eyre::eyre!("Invalid encryption nonce"))?;
    let key = aead::LessSafeKey::new(key);
    let mut bytes = Zeroizing::new(ciphertext.to_vec());
    let length = key
        .open_in_place(nonce, aead::Aad::empty(), &mut bytes)
        .map_err(|_| eyre::eyre!("Promis ciphertext authentication failed"))?
        .len();
    bytes.truncate(length);
    Ok(bytes)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SealedKeys {
    sealed: Bytes,
    nonce: Bytes,
    enclave_ephemeral_pubkey: B256,
}

pub async fn derive(rpc: &impl Rpc, wallet: &Wallet) -> Result<Keys> {
    // Cryptographic randomness for the wallet's one-use encryption secret only.
    let secret = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &SystemRandom::new())
        .map_err(|_| eyre::eyre!("Ephemeral key generation failed"))?;
    let public = secret
        .compute_public_key()
        .map_err(|_| eyre::eyre!("Ephemeral public key failed"))?;
    let public = B256::from_slice(public.as_ref());
    let hash = eip191_hash(&derive_account_keys_message(
        Ledger::Promis,
        wallet.address,
        public,
    ));
    let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = wallet
        .key
        .sign_prehash(hash.as_slice())
        .map_err(|_| eyre::eyre!("Owner signature failed"))?;
    let mut signature = signature.to_bytes().to_vec();
    signature.push(27 + recovery.to_byte());
    let response = rpc
        .request(
            "rudis_deriveKeys",
            serde_json::json!([
                "Promis",
                wallet.address,
                public,
                format!("0x{}", hex::encode(signature))
            ]),
        )
        .await?;
    let response: SealedKeys = serde_json::from_value(response)
        .map_err(|_| eyre::eyre!("Invalid sealed keys response"))?;
    let peer =
        agreement::UnparsedPublicKey::new(&agreement::X25519, response.enclave_ephemeral_pubkey);
    let shared =
        agreement::agree_ephemeral(secret, &peer, |shared| Zeroizing::new(shared.to_vec()))
            .map_err(|_| eyre::eyre!("Enclave key agreement failed"))?;
    let key = hkdf32(public.as_slice(), &shared, b"outbe/tee/dkg-share/v1")?;
    let plaintext = decrypt(key.as_ref(), &response.nonce, &response.sealed)?;
    ensure!(plaintext.len() == 64, "Invalid Promis keys length");
    let mut keys = Zeroizing::new([0; 64]);
    keys.copy_from_slice(&plaintext);
    Ok(Keys(keys))
}

impl Keys {
    pub fn mac(
        &self,
        account: Address,
        op: PromisOp,
        amount: U256,
        nonce: u64,
        chain: u64,
    ) -> B256 {
        let key = hmac::Key::new(hmac::HMAC_SHA256, &self.0[32..]);
        let mut preimage = b"outbe/promis/modify/v1".to_vec();
        preimage.extend_from_slice(account.as_slice());
        preimage.push(op as u8);
        preimage.extend_from_slice(&amount.to_be_bytes::<32>());
        preimage.extend_from_slice(&nonce.to_be_bytes());
        preimage.extend_from_slice(&U256::from(chain).to_be_bytes::<32>());
        B256::from_slice(hmac::sign(&key, &preimage).as_ref())
    }

    pub fn balance(&self, account: Address, blob: &[u8]) -> Result<U256> {
        if blob.is_empty() {
            return Ok(U256::ZERO);
        }
        ensure!(blob.len() == 56, "Invalid Promis balance ciphertext length");
        let mut input = account.as_slice().to_vec();
        input.push(0);
        input.extend_from_slice(&blob[..8]);
        let nonce = hkdf32(&self.0[..32], &input, b"outbe/promis/nonce/v1")?;
        let plaintext = decrypt(&self.0[..32], &nonce[..12], &blob[8..])?;
        ensure!(plaintext.len() == 32, "Invalid decrypted balance length");
        Ok(U256::from_be_slice(&plaintext))
    }

    #[cfg(test)]
    pub fn for_test() -> Self {
        Self(Zeroizing::new([7; 64]))
    }
}
