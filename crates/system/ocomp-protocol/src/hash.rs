use alloy_primitives::{keccak256, B256};

use crate::{error::ProtocolError, registry::HashDomain};

/// Build the exact registered-domain preimage consumed by Keccak-256.
pub fn framed_preimage(domain: HashDomain, payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let domain_bytes = domain.as_str().as_bytes();
    debug_assert!(domain_bytes.is_ascii());
    let domain_len =
        u16::try_from(domain_bytes.len()).map_err(|_| ProtocolError::IntegerOverflow {
            what: "hash domain length",
        })?;
    let payload_len = u32::try_from(payload.len()).map_err(|_| ProtocolError::IntegerOverflow {
        what: "hash payload length",
    })?;
    let total_len = 2_usize
        .checked_add(domain_bytes.len())
        .and_then(|value| value.checked_add(4))
        .and_then(|value| value.checked_add(payload.len()))
        .ok_or(ProtocolError::IntegerOverflow {
            what: "hash preimage length",
        })?;

    let mut preimage = Vec::new();
    preimage
        .try_reserve_exact(total_len)
        .map_err(|_| ProtocolError::AllocationFailed {
            what: "hash preimage",
            bytes: total_len,
        })?;
    preimage.extend_from_slice(&domain_len.to_be_bytes());
    preimage.extend_from_slice(domain_bytes);
    preimage.extend_from_slice(&payload_len.to_be_bytes());
    preimage.extend_from_slice(payload);
    Ok(preimage)
}

pub fn hash_framed(domain: HashDomain, payload: &[u8]) -> Result<B256, ProtocolError> {
    Ok(keccak256(framed_preimage(domain, payload)?))
}

pub fn verify_framed_hash(
    domain: HashDomain,
    payload: &[u8],
    expected: B256,
) -> Result<(), ProtocolError> {
    if hash_framed(domain, payload)? == expected {
        Ok(())
    } else {
        Err(ProtocolError::HashMismatch)
    }
}

/// Expands, inside an `impl` block, to `pub fn $method(&self, limits)`: the
/// framed hash of the canonical encoding of `self` under `HashDomain::$domain`.
/// A check argument makes the method call `self.$check(limits)` or
/// `self.$check()` first.
macro_rules! framed_identity_hash {
    ($(#[$meta:meta])* $method:ident, $domain:ident) => {
        $(#[$meta])*
        pub fn $method(
            &self,
            limits: &$crate::schema::SchemaLimits,
        ) -> Result<::alloy_primitives::B256, $crate::error::ProtocolError> {
            $crate::hash::hash_framed(
                $crate::registry::HashDomain::$domain,
                &self.encode_canonical(limits)?,
            )
        }
    };
    ($(#[$meta:meta])* $method:ident, $domain:ident, $check:ident(limits)) => {
        $(#[$meta])*
        pub fn $method(
            &self,
            limits: &$crate::schema::SchemaLimits,
        ) -> Result<::alloy_primitives::B256, $crate::error::ProtocolError> {
            self.$check(limits)?;
            $crate::hash::hash_framed(
                $crate::registry::HashDomain::$domain,
                &self.encode_canonical(limits)?,
            )
        }
    };
    ($(#[$meta:meta])* $method:ident, $domain:ident, $check:ident()) => {
        $(#[$meta])*
        pub fn $method(
            &self,
            limits: &$crate::schema::SchemaLimits,
        ) -> Result<::alloy_primitives::B256, $crate::error::ProtocolError> {
            self.$check()?;
            $crate::hash::hash_framed(
                $crate::registry::HashDomain::$domain,
                &self.encode_canonical(limits)?,
            )
        }
    };
}

pub(crate) use framed_identity_hash;
