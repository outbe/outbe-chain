use alloy_primitives::{keccak256, U256};

/// Encode Solidity bytes independently of the production storage encoder.
pub fn solidity_bytes_storage_slots(base: U256, encoded: &[u8]) -> Vec<(U256, U256)> {
    if encoded.len() <= 31 {
        let mut inline = [0_u8; 32];
        inline[..encoded.len()].copy_from_slice(encoded);
        inline[31] = (encoded.len() * 2) as u8;
        return vec![(base, U256::from_be_bytes(inline))];
    }

    let data_base = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
    std::iter::once((base, U256::from(encoded.len() * 2 + 1)))
        .chain(encoded.chunks(32).enumerate().map(|(index, chunk)| {
            let mut word = [0_u8; 32];
            word[..chunk.len()].copy_from_slice(chunk);
            (data_base + U256::from(index), U256::from_be_bytes(word))
        }))
        .collect()
}
