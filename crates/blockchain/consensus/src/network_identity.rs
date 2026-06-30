//! Trusted network identity — the BLS threshold group public key a follower
//! anchors finalization verification on.
//!
//! For outbe the per-epoch committee (and thus the finalize-cert verifier)
//! changes on every reshare, so this anchor only *bootstraps* the start epoch;
//! later epochs' committees are trusted via the finalized-boundary chain (the
//! boundary block announcing a new committee is itself finalized by the
//! already-trusted committee). See the `follow/` module. Mirrors Tempo's
//! `chainspec::NetworkIdentity`.

use commonware_codec::ReadExt as _;
use commonware_cryptography::bls12381::primitives::variant::{MinSig, Variant};

/// A trusted consensus anchor: the BLS12-381 MinSig threshold group public key
/// together with the first epoch it is expected to verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkIdentity {
    /// First epoch for which `identity` is expected to verify finalizations.
    pub from_epoch: u64,
    /// BLS12-381 MinSig threshold group public key.
    pub identity: <MinSig as Variant>::Public,
}

impl NetworkIdentity {
    /// Parse from a hex-encoded group public key (with or without a `0x` prefix)
    /// and the epoch it is valid from.
    pub fn from_hex(identity_hex: &str, from_epoch: u64) -> eyre::Result<Self> {
        let stripped = identity_hex.strip_prefix("0x").unwrap_or(identity_hex);
        let bytes = hex::decode(stripped)
            .map_err(|e| eyre::eyre!("invalid network identity hex: {e}"))?;
        let identity = <MinSig as Variant>::Public::read(&mut bytes.as_slice())
            .map_err(|e| eyre::eyre!("invalid BLS12-381 network identity: {e}"))?;
        Ok(Self {
            from_epoch,
            identity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_codec::Encode as _;

    #[test]
    fn network_identity_hex_roundtrip() {
        // A real MinSig group public key (the constant term of a DKG polynomial).
        let dkg = crate::bls::bootstrap_dkg(4).unwrap();
        let public = dkg.polynomial.public().clone();
        let hex_id = hex::encode(public.encode());

        let parsed = NetworkIdentity::from_hex(&hex_id, 7).unwrap();
        assert_eq!(parsed.from_epoch, 7);
        assert_eq!(parsed.identity, public);

        // 0x prefix is accepted.
        let parsed_0x = NetworkIdentity::from_hex(&format!("0x{hex_id}"), 0).unwrap();
        assert_eq!(parsed_0x.identity, public);

        // garbage is rejected.
        assert!(NetworkIdentity::from_hex("not-hex", 0).is_err());
        assert!(NetworkIdentity::from_hex("0xdead", 0).is_err());
    }
}
