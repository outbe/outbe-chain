//! NFT signing keys and the static-ECDH consent box on Grumpkin.
//!
//! A [`Signer`] encapsulates the NFT secret and exposes an [`OwnerSeed`] plus
//! signing operations. [`Signer::local`] generates a fresh key and nonce.
//! [`Signer::issue_for_remote`] derives a key for the recipient and destroys
//! the server's secrets; [`Signer::from_exchange`] reconstructs that same key
//! on the client. Both sides derive `KDF(shared_point.x, nonce)`, so the NFT
//! secret never crosses the wire.

use ark_std::rand::Rng;
use ark_std::UniformRand;
use zeroize::Zeroize;

use crate::error::Error;
use crate::primitive::curve::{base_to_scalar, Affine, Scalar};
use crate::primitive::{exchange, hash, kdf, signature};
use crate::Fr;

/// A stored secret scalar that wipes itself on drop and is never implicitly copied.
///
/// The underlying arkworks scalar is `Copy` and implements [`Zeroize`], but
/// cannot wipe itself on drop. Keeping it in this non-`Copy` wrapper protects
/// the stored key. Read it only through [`expose`](Self::expose); arithmetic
/// may still produce transient scalar copies outside the wrapper.
pub struct SecretScalar(Scalar);

impl SecretScalar {
    pub fn new(secret: Scalar) -> Self {
        Self(secret)
    }

    /// Borrow the inner scalar for curve arithmetic. Do not copy it out.
    pub fn expose(&self) -> &Scalar {
        &self.0
    }
}

impl Drop for SecretScalar {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl zeroize::ZeroizeOnDrop for SecretScalar {}

/// The public key and nonce committed by `derivedOwner`.
pub struct OwnerSeed {
    pub pk: Affine,
    pub nonce: Fr,
}

impl OwnerSeed {
    pub fn derive_owner(&self) -> Result<Fr, Error> {
        hash::derive_owner(&self.pk, self.nonce)
    }
}

/// A retained NFT secret key, held in a wiping [`SecretScalar`].
pub struct NftSecret {
    sk: SecretScalar,
}

impl NftSecret {
    /// Wrap a raw signing secret so it zeroizes on drop.
    pub fn new(sk: Scalar) -> Self {
        Self {
            sk: SecretScalar::new(sk),
        }
    }

    /// `pk = [sk]·G`.
    pub fn public_key(&self) -> Result<Affine, Error> {
        signature::public_key(self.sk.expose())
    }
}

/// NFT signing authority without exposing its secret key.
///
/// Custom implementations can back signing with an HSM or a remote signer.
/// The nonce lives in the owner seed so it cannot drift from the signer.
pub trait NftSigner {
    fn owner_seed(&self) -> &OwnerSeed;
    fn sign<R: Rng>(&self, rng: &mut R, payload: Fr) -> Result<[u8; 64], Error>;
    fn public_key(&self) -> Affine {
        self.owner_seed().pk
    }
}

/// An NFT signer with an encapsulated secret.
///
/// Construct locally, reconstruct through the consent box, or bind a stored
/// [`NftSecret`]. [`secret`](Self::secret) and [`into_secret`](Self::into_secret)
/// hand the wrapped key over explicitly when it must be retained elsewhere.
pub struct Signer {
    secret: NftSecret,
    seed: OwnerSeed,
}

impl Signer {
    /// Local self-issuance: generate a fresh NFT key and nonce.
    pub fn local(rng: &mut impl Rng) -> Result<Self, Error> {
        let (sk, pk) = signature::keypair(rng);
        let nonce = Fr::rand(rng);
        Ok(Self {
            secret: NftSecret::new(sk),
            seed: OwnerSeed { pk, nonce },
        })
    }

    /// Server side: derive an NFT key using the recipient's consent public key.
    /// Returns only public artifacts; the ephemeral and NFT secrets wipe on drop.
    pub fn issue_for_remote(
        rng: &mut impl Rng,
        consent_pk: &Affine,
    ) -> Result<(OwnerSeed, Affine), Error> {
        let (opaque_sk, opaque_pk) = exchange::random_keypair(rng);
        let nonce = Fr::rand(rng);
        let opaque_sk = SecretScalar::new(opaque_sk);
        let shared = exchange::shared(opaque_sk.expose(), consent_pk)?;
        let nft_sk = SecretScalar::new(derive_nft_secret(shared, nonce)?);
        let pk = signature::public_key(nft_sk.expose())?;
        Ok((OwnerSeed { pk, nonce }, opaque_pk))
    }

    /// Client side: reconstruct the NFT secret from the consent-box transcript.
    pub fn from_exchange(
        consent_sk: &Scalar,
        opaque_pk: &Affine,
        nonce: Fr,
    ) -> Result<Self, Error> {
        let shared = exchange::shared(consent_sk, opaque_pk)?;
        let secret = NftSecret::new(derive_nft_secret(shared, nonce)?);
        let pk = secret.public_key()?;
        Ok(Self {
            secret,
            seed: OwnerSeed { pk, nonce },
        })
    }

    /// Bind an existing NFT secret to its owner-commitment nonce.
    pub fn from_secret(secret: NftSecret, nonce: Fr) -> Result<Self, Error> {
        let pk = secret.public_key()?;
        Ok(Self {
            secret,
            seed: OwnerSeed { pk, nonce },
        })
    }

    pub fn secret(&self) -> &NftSecret {
        &self.secret
    }

    pub fn into_secret(self) -> NftSecret {
        self.secret
    }
}

impl NftSigner for Signer {
    fn owner_seed(&self) -> &OwnerSeed {
        &self.seed
    }

    fn sign<R: Rng>(&self, rng: &mut R, payload: Fr) -> Result<[u8; 64], Error> {
        signature::sign(rng, self.secret.sk.expose(), payload)
    }
}

/// Map the consent-box KDF output into Grumpkin's scalar field.
fn derive_nft_secret(shared: Fr, nonce: Fr) -> Result<Scalar, Error> {
    let kdf_out = kdf::derive(&[shared, nonce])?;
    Ok(base_to_scalar(&kdf_out))
}
