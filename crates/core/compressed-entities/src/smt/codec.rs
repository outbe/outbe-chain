use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_sparse_merkle_tree_v061::{traits::Hasher, H256};
use std::cell::RefCell;

use crate::{TAG_SMT_BASE, TAG_SMT_NORMAL, TAG_SMT_ZERO};

const HASH_ERROR_BYTES: [u8; 32] = [u8::MAX; 32];

thread_local! {
    static HASH_FAILURES: RefCell<Vec<Option<crate::CommitmentError>>> = const { RefCell::new(Vec::new()) };
}

/// Keep the typed cause local to one CKB operation, including nested operations.
pub(super) fn hash_scope<T>(
    operation: impl FnOnce() -> Result<T, super::TreeError>,
) -> Result<T, super::TreeError> {
    struct Scope;
    impl Drop for Scope {
        fn drop(&mut self) {
            HASH_FAILURES.with(|failures| {
                failures.borrow_mut().pop();
            });
        }
    }
    HASH_FAILURES.with(|failures| failures.borrow_mut().push(None));
    let scope = Scope;
    let result = operation();
    let cause =
        HASH_FAILURES.with(|failures| failures.borrow_mut().last_mut().and_then(Option::take));
    drop(scope);
    match cause {
        Some(error) => Err(super::TreeError::Poseidon(error)),
        None => result,
    }
}

pub(crate) fn hash_error() -> H256 {
    H256::from(HASH_ERROR_BYTES)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TranscriptItem {
    Byte(u8),
    Word(H256),
}

/// CKB's infallible hasher seam backed by the CES2 Poseidon transcript codec.
#[derive(Default)]
pub(crate) struct PoseidonCkbHasher {
    transcript: Vec<TranscriptItem>,
}

impl PoseidonCkbHasher {
    fn finish_with(
        self,
        hash: impl FnOnce(u64, &[Fr]) -> Result<Fr, crate::CommitmentError>,
    ) -> H256 {
        let Ok((tag, inputs)) = classify(&self.transcript) else {
            return hash_error();
        };
        let output = match hash(tag, &inputs) {
            Ok(output) => output,
            Err(error) => {
                HASH_FAILURES.with(|failures| {
                    if let Some(slot) = failures.borrow_mut().last_mut() {
                        if slot.is_none() {
                            *slot = Some(error);
                        }
                    }
                });
                return hash_error();
            }
        };
        if output == Fr::from(0_u64) {
            return hash_error();
        }
        H256::from(field_to_be32(output))
    }
}

impl Hasher for PoseidonCkbHasher {
    fn write_h256(&mut self, h: &H256) {
        self.transcript.push(TranscriptItem::Word(*h));
    }

    fn write_byte(&mut self, b: u8) {
        self.transcript.push(TranscriptItem::Byte(b));
    }

    fn finish(self) -> H256 {
        self.finish_with(crate::commitment::poseidon)
    }
}

fn classify(transcript: &[TranscriptItem]) -> Result<(u64, Vec<Fr>), ()> {
    match transcript {
        [TranscriptItem::Byte(base_height), TranscriptItem::Word(base_key), TranscriptItem::Word(base_value)] => {
            Ok((
                TAG_SMT_BASE,
                vec![
                    Fr::from(*base_height),
                    field(*base_key)?,
                    field(*base_value)?,
                ],
            ))
        }
        [TranscriptItem::Byte(1), TranscriptItem::Byte(height), TranscriptItem::Word(node_key), TranscriptItem::Word(left), TranscriptItem::Word(right)] => {
            Ok((
                TAG_SMT_NORMAL,
                vec![
                    Fr::from(*height),
                    field(*node_key)?,
                    field(*left)?,
                    field(*right)?,
                ],
            ))
        }
        [TranscriptItem::Byte(2), TranscriptItem::Word(base_node), TranscriptItem::Word(zero_bits), TranscriptItem::Byte(zero_count)] => {
            Ok((
                TAG_SMT_ZERO,
                vec![
                    field(*base_node)?,
                    field(*zero_bits)?,
                    Fr::from(*zero_count),
                ],
            ))
        }
        _ => Err(()),
    }
}

pub(super) fn field(value: H256) -> Result<Fr, ()> {
    if value == hash_error() {
        return Err(());
    }
    let bytes: [u8; 32] = value.into();
    let field = Fr::from_be_bytes_mod_order(&bytes);
    if field_to_be32(field) != bytes {
        return Err(());
    }
    Ok(field)
}

pub(super) fn is_canonical(value: H256) -> bool {
    field(value).is_ok()
}

fn field_to_be32(value: Fr) -> [u8; 32] {
    let bytes = value.into_bigint().to_bytes_be();
    let mut output = [0_u8; 32];
    output[32 - bytes.len()..].copy_from_slice(&bytes);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unexpected_transcript_and_hash_failure_are_poisoned() {
        let mut unexpected = PoseidonCkbHasher::default();
        unexpected.write_byte(9);
        assert_eq!(unexpected.finish(), hash_error());

        let mut valid = PoseidonCkbHasher::default();
        valid.write_byte(0);
        valid.write_h256(&H256::zero());
        valid.write_h256(&H256::from([0_u8; 32]));
        assert_eq!(
            valid.finish_with(|_, _| Err(crate::CommitmentError::InputTooLong)),
            hash_error()
        );
    }

    #[test]
    fn noncanonical_or_poison_words_are_poisoned() {
        let modulus = H256::from([
            0x30, 0x64, 0x4e, 0x72, 0xe1, 0x31, 0xa0, 0x29, 0xb8, 0x50, 0x45, 0xb6, 0x81, 0x81,
            0x58, 0x5d, 0x28, 0x33, 0xe8, 0x48, 0x79, 0xb9, 0x70, 0x91, 0x43, 0xe1, 0xf5, 0x93,
            0xf0, 0x00, 0x00, 0x01,
        ]);
        for invalid in [hash_error(), modulus] {
            let mut hasher = PoseidonCkbHasher::default();
            hasher.write_byte(0);
            hasher.write_h256(&H256::zero());
            hasher.write_h256(&invalid);
            assert_eq!(hasher.finish(), hash_error());
        }
    }

    #[test]
    fn sentinel_does_not_invent_backend_provenance_or_leak_between_scopes() {
        let malformed = hash_scope(|| {
            let mut hasher = PoseidonCkbHasher::default();
            hasher.write_byte(9);
            assert_eq!(hasher.finish(), hash_error());
            Err::<(), _>(super::super::TreeError::TreeHashError)
        })
        .unwrap_err();
        assert!(!malformed.is_infrastructure());
        let zero = hash_scope(|| {
            let mut hasher = PoseidonCkbHasher::default();
            hasher.write_byte(0);
            hasher.write_h256(&H256::zero());
            hasher.write_h256(&H256::zero());
            assert_eq!(hasher.finish_with(|_, _| Ok(Fr::from(0_u64))), hash_error());
            Err::<(), _>(super::super::TreeError::TreeHashError)
        })
        .unwrap_err();
        assert!(!zero.is_infrastructure());
        let failed = {
            let _failure = outbe_poseidon::testing::fail_after(0);
            hash_scope(|| {
                let mut hasher = PoseidonCkbHasher::default();
                hasher.write_byte(0);
                hasher.write_h256(&H256::zero());
                hasher.write_h256(&H256::zero());
                assert_eq!(hasher.finish(), hash_error());
                Ok(())
            })
            .unwrap_err()
        };
        assert!(failed.is_infrastructure());
        assert!(hash_scope(|| Ok(())).is_ok());
        assert!(std::thread::spawn(|| hash_scope(|| Ok(())).is_ok())
            .join()
            .unwrap());
    }
}
