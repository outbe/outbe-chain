use crypto::signature::Verifier as _;
use crypto::{BlsEnvelope, ExtendedSignature, RepositoryKey, Signature, VerifyingKey};
use oid::Oid;

/// BLS entries carry a proof produced only after checking the actual raw commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntrySignature {
    Ed25519(Signature),
    Bls12381G2(Box<VerifiedBls>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBls {
    entry: Oid,
    tree: Oid,
    envelope: BlsEnvelope,
}

impl VerifiedBls {
    pub fn envelope(&self) -> &BlsEnvelope {
        &self.envelope
    }
}

impl EntrySignature {
    pub fn legacy(signature: ExtendedSignature) -> ExtendedSignature<RepositoryKey, Self> {
        let (key, signature) = signature.into_pair();
        ExtendedSignature::new(key.into(), Self::Ed25519(signature))
    }

    #[cfg(feature = "git2")]
    pub(crate) fn bls(
        entry: Oid,
        repository: [u8; 20],
        context: [u8; 32],
        commit: &metadata::commit::bls::SignedCommit,
    ) -> Result<ExtendedSignature<RepositoryKey, Self>, crypto::repository::Error> {
        let envelope = BlsEnvelope::from_hex(commit.envelope())?;
        let unsigned = commit.unsigned();
        crypto::RepositorySignature::from(envelope.clone()).verify_cob(
            envelope.key().into(),
            repository,
            context,
            unsigned.tree(),
            unsigned.as_bytes(),
        )?;
        let tree = Oid::from_sha1(unsigned.tree());
        Ok(ExtendedSignature::new(
            envelope.key().into(),
            Self::Bls12381G2(Box::new(VerifiedBls {
                entry,
                tree,
                envelope,
            })),
        ))
    }

    pub(super) fn verify(&self, key: &RepositoryKey, entry: &Oid, tree: &Oid) -> bool {
        match self {
            Self::Ed25519(signature) => VerifyingKey::try_from(key)
                .and_then(|verifier| verifier.verify(tree.as_ref(), signature))
                .is_ok(),
            Self::Bls12381G2(verified) => {
                verified.entry == *entry
                    && verified.tree == *tree
                    && RepositoryKey::from(verified.envelope.key()) == *key
            }
        }
    }
}
