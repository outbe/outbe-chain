use alloy_primitives::{Address, B256};
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_poseidon::{Poseidon2, PoseidonHasher};
use outbe_primitives::time::WorldwideDay;

pub use outbe_primitives::wwd_entity_id::WwdEntityId;

pub fn derive_poseidon_digest(owner: Address, worldwide_day: WorldwideDay) -> B256 {
    let digest = Poseidon2::<Fr>::new()
        .hash(&[
            Fr::from_be_bytes_mod_order(owner.as_slice()),
            Fr::from(worldwide_day.value()),
        ])
        // PoseidonError is Infallible, so this boundary cannot fail.
        .unwrap();
    let encoded = digest.into_bigint().to_bytes_be();
    let mut bytes = [0u8; 32];
    bytes[32 - encoded.len()..].copy_from_slice(&encoded);
    B256::new(bytes)
}

pub fn derive_poseidon_entity_id(owner: Address, worldwide_day: WorldwideDay) -> WwdEntityId {
    WwdEntityId::from_day_and_digest(worldwide_day, derive_poseidon_digest(owner, worldwide_day))
}

#[cfg(test)]
mod tests {
    use super::{derive_poseidon_digest, derive_poseidon_entity_id, WwdEntityId};
    use alloy_primitives::{Address, B256};
    use outbe_primitives::time::WorldwideDay;

    #[test]
    fn poseidon_identity_matches_pinned_noir_vectors() {
        let cases = [
            (
                0x42,
                0x01020304,
                "14edf11805beb34363ddabb6c010e0b5883bfe90d9e06ff2eeffa66f61683c0b",
                "0102030405beb34363ddabb6c010e0b5883bfe90d9e06ff2eeffa66f61683c0b",
            ),
            (
                0x24,
                0x0a0b0c0d,
                "15f0bc32905aebee0649e038f8b64544e2792c7e56193e513c8a5856a0e870ad",
                "0a0b0c0d905aebee0649e038f8b64544e2792c7e56193e513c8a5856a0e870ad",
            ),
        ];
        for (owner, day, digest, identity) in cases {
            let owner = Address::repeat_byte(owner);
            let day = WorldwideDay::new(day);
            assert_eq!(
                derive_poseidon_digest(owner, day),
                digest.parse::<B256>().unwrap(),
            );
            assert_eq!(
                derive_poseidon_entity_id(owner, day),
                identity.parse::<WwdEntityId>().unwrap(),
            );
        }
    }
}
