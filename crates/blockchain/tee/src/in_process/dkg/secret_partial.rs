//! Flat partial signatures need wiping too: a threshold set reconstructs the root.
use commonware_cryptography::bls12381::primitives::variant::{MinSig, PartialSignature};
use std::{mem::ManuallyDrop, ops::Deref};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub(super) struct SecretPartial(ManuallyDrop<PartialSignature<MinSig>>);
impl SecretPartial {
    pub(super) fn new(value: PartialSignature<MinSig>) -> Self {
        Self(ManuallyDrop::new(value))
    }
}
impl Deref for SecretPartial {
    type Target = PartialSignature<MinSig>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for SecretPartial {
    #[allow(unsafe_code)] // Audited flat Commonware MinSig partial: Participant + inline blst G1, no pointers/destructors.
    fn drop(&mut self) {
        // SAFETY: exclusive live allocation; ManuallyDrop prevents reading the wiped
        // Participant representation or running a destructor after its bytes are cleared.
        unsafe {
            std::slice::from_raw_parts_mut(
                std::ptr::from_mut(&mut self.0).cast::<u8>(),
                std::mem::size_of_val(&self.0),
            )
            .zeroize();
        }
    }
}
impl ZeroizeOnDrop for SecretPartial {}
impl std::fmt::Debug for SecretPartial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretPartial([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_cryptography::bls12381::primitives::ops::threshold;
    #[test]
    #[allow(unsafe_code)] // Inspect only a live MaybeUninit allocation after its contained destructor ran.
    fn in_process_partial_drop_wipes_its_original_storage() {
        let shares = outbe_simplex::bls::bootstrap_dkg(4).unwrap();
        let partial = threshold::sign_message::<MinSig>(&shares.shares[0], b"test", b"root");
        let mut storage = std::mem::MaybeUninit::new(SecretPartial::new(partial));
        let pointer = storage.as_mut_ptr();
        // SAFETY: exactly one destructor invocation on the initialized value; storage
        // remains allocated, and Drop wrote every byte (including padding) to zero.
        unsafe {
            std::ptr::drop_in_place(pointer);
            let bytes = std::slice::from_raw_parts(
                pointer.cast::<u8>(),
                std::mem::size_of::<SecretPartial>(),
            );
            assert!(bytes.iter().all(|byte| *byte == 0));
        }
    }
}
