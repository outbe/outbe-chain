//! Flat partial signatures need wiping too: a threshold set reconstructs the root.
use commonware_cryptography::bls12381::primitives::variant::{MinSig, PartialSignature};
use std::{mem::ManuallyDrop, ops::Deref};
use zeroize::ZeroizeOnDrop;

const _: () = assert!(!std::mem::needs_drop::<PartialSignature<MinSig>>());

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
        // SAFETY: the pinned MinSig partial is flat and needs no destructor.
        // This raw-pointer primitive also wipes padding without forming references to it.
        unsafe {
            zeroize::zeroize_flat_type(&raw mut *self.0);
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
        let mut storage = std::mem::MaybeUninit::<SecretPartial>::uninit();
        let pointer = storage.as_mut_ptr();
        // SAFETY: aligned, live storage is prefilled before initializing the value.
        // No byte slice is formed until Drop initializes every byte, including padding.
        unsafe {
            std::ptr::write_bytes(
                pointer.cast::<u8>(),
                0xa5,
                std::mem::size_of::<SecretPartial>(),
            );
            pointer.write(SecretPartial::new(partial));
            std::ptr::drop_in_place(pointer);
            let bytes = std::slice::from_raw_parts(
                pointer.cast::<u8>(),
                std::mem::size_of::<SecretPartial>(),
            );
            assert!(bytes.iter().all(|byte| *byte == 0));
        }
    }
}
