//! Degree-one Pedersen VSS and authenticated per-recipient transport.
//! Protocol separation on one host; no claim of OS isolation or secure erasure.
use crate::crypto::*;
use ark_ec::CurveGroup;
use ark_ed_on_bn254::{EdwardsAffine, Fr};
use ark_ff::{Field, UniformRand, Zero};
use chacha20poly1305::{
    aead::{Aead, Payload},
    ChaCha20Poly1305, KeyInit,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use x25519_dalek::{PublicKey, StaticSecret};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub id: String,
    pub encryption: String,
    pub signing: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keys {
    pub public: Identity,
    encryption: String,
    signing: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Share {
    pub x: u32,
    pub y: String,
    pub z: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Polynomial {
    pub id: String,
    pub epoch: u64,
    pub points: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub context: String,
    pub recipient: String,
    pub ephemeral: String,
    pub nonce: String,
    pub ciphertext: String,
}

pub fn bytes32(s: &str) -> Result<[u8; 32]> {
    Ok(hex::decode(s)?
        .try_into()
        .map_err(|_| "expected 32 bytes")?)
}
pub fn digest(value: &impl Serialize) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}
pub fn point(s: &str) -> Result<EdwardsAffine> {
    decode(&hex::decode(s)?)
}
pub fn point_hex(p: EdwardsAffine) -> Result<String> {
    Ok(hex::encode(encode(&p)?))
}
pub fn keygen(dir: &Path, id: String) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let encryption = StaticSecret::random_from_rng(OsRng);
    let signing = SigningKey::generate(&mut OsRng);
    let public = Identity {
        id,
        encryption: hex::encode(PublicKey::from(&encryption).as_bytes()),
        signing: hex::encode(signing.verifying_key().as_bytes()),
    };
    write_private_json(
        &dir.join("keys.private.json"),
        &Keys {
            public: public.clone(),
            encryption: hex::encode(encryption.to_bytes()),
            signing: hex::encode(signing.to_bytes()),
        },
    )?;
    write_json(&dir.join("identity.json"), &public)
}
pub fn keys(dir: &Path) -> Result<Keys> {
    Ok(serde_json::from_slice(&std::fs::read(
        dir.join("keys.private.json"),
    )?)?)
}
pub fn seal(recipient: &Identity, context: &str, plaintext: &[u8]) -> Result<Envelope> {
    let ephemeral = StaticSecret::random_from_rng(OsRng);
    let epk = PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(&PublicKey::from(bytes32(&recipient.encryption)?));
    if !shared.was_contributory() {
        return Err("invalid recipient key".into());
    }
    let mut kdf = Sha256::new();
    kdf.update(b"OUTBE-POC-VSS-AEAD-v1");
    kdf.update(shared.as_bytes());
    kdf.update(epk.as_bytes());
    kdf.update(bytes32(&recipient.encryption)?);
    kdf.update(context);
    let cipher = ChaCha20Poly1305::new(&kdf.finalize());
    let mut nonce = [0; 12];
    OsRng.fill_bytes(&mut nonce);
    let aad = format!("{context}:{}", recipient.id);
    let ciphertext = cipher
        .encrypt(
            (&nonce).into(),
            Payload {
                msg: plaintext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| "AEAD seal")?;
    Ok(Envelope {
        context: context.into(),
        recipient: recipient.id.clone(),
        ephemeral: hex::encode(epk.as_bytes()),
        nonce: hex::encode(nonce),
        ciphertext: hex::encode(ciphertext),
    })
}
pub fn unseal(keys: &Keys, e: &Envelope, context: &str) -> Result<Vec<u8>> {
    if e.context != context || e.recipient != keys.public.id {
        return Err("wrong envelope context/recipient".into());
    }
    let private = StaticSecret::from(bytes32(&keys.encryption)?);
    let shared = private.diffie_hellman(&PublicKey::from(bytes32(&e.ephemeral)?));
    if !shared.was_contributory() {
        return Err("invalid ephemeral key".into());
    }
    let mut kdf = Sha256::new();
    kdf.update(b"OUTBE-POC-VSS-AEAD-v1");
    kdf.update(shared.as_bytes());
    kdf.update(bytes32(&e.ephemeral)?);
    kdf.update(bytes32(&keys.public.encryption)?);
    kdf.update(context);
    let cipher = ChaCha20Poly1305::new(&kdf.finalize());
    let nonce: [u8; 12] = hex::decode(&e.nonce)?
        .try_into()
        .map_err(|_| "nonce length")?;
    let aad = format!("{context}:{}", keys.public.id);
    cipher
        .decrypt(
            (&nonce).into(),
            Payload {
                msg: &hex::decode(&e.ciphertext)?,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| "invalid AEAD ciphertext".into())
}
pub fn sign(keys: &Keys, value: &impl Serialize) -> Result<String> {
    Ok(hex::encode(
        SigningKey::from_bytes(&bytes32(&keys.signing)?)
            .sign(digest(value)?.as_bytes())
            .to_bytes(),
    ))
}
pub fn verify_signature(id: &Identity, value: &impl Serialize, s: &str) -> Result<()> {
    VerifyingKey::from_bytes(&bytes32(&id.signing)?)?.verify(
        digest(value)?.as_bytes(),
        &Signature::from_slice(&hex::decode(s)?)?,
    )?;
    Ok(())
}
pub fn deal(id: String, epoch: u64, y: Fr, z: Fr, n: u32) -> Result<(Polynomial, Vec<Share>)> {
    let a = Fr::rand(&mut OsRng);
    let b = Fr::rand(&mut OsRng);
    let p = Polynomial {
        id,
        epoch,
        points: vec![
            point_hex(commit(&scalar_integer(y), z)?)?,
            point_hex(commit(&scalar_integer(a), b)?)?,
        ],
    };
    let shares = (1..=n)
        .map(|x| Share {
            x,
            y: scalar_integer(y + a * Fr::from(x)).to_string(),
            z: scalar_integer(z + b * Fr::from(x)).to_string(),
        })
        .collect();
    Ok((p, shares))
}
pub fn eval(p: &Polynomial, x: u32) -> Result<EdwardsAffine> {
    if p.points.len() != 2 || x == 0 {
        return Err("expected degree-one VSS, nonzero coordinate".into());
    }
    Ok((point(&p.points[0])? + point(&p.points[1])? * Fr::from(x)).into_affine())
}
pub fn check(p: &Polynomial, s: &Share) -> Result<()> {
    if commit(&integer(&s.y)?, scalar(&integer(&s.z)?)?)? != eval(p, s.x)? {
        return Err("VSS equation failed".into());
    }
    Ok(())
}
pub fn lagrange(xs: &[u32], i: usize) -> Result<Fr> {
    let mut a = Fr::from(1u32);
    let mut b = a;
    for (j, x) in xs.iter().enumerate() {
        if i != j {
            a *= -Fr::from(*x);
            b *= Fr::from(xs[i]) - Fr::from(*x);
        }
    }
    Ok(a * b.inverse().ok_or("duplicate share coordinate")?)
}
pub fn recover(shares: &[Share]) -> Result<(Fr, Fr)> {
    if shares.len() < 2 {
        return Err("threshold is two".into());
    }
    let xs = shares.iter().map(|s| s.x).collect::<Vec<_>>();
    let mut a = Fr::zero();
    let mut b = a;
    for (i, s) in shares.iter().enumerate() {
        let l = lagrange(&xs, i)?;
        a += l * scalar(&integer(&s.y)?)?;
        b += l * scalar(&integer(&s.z)?)?;
    }
    Ok((a, b))
}
