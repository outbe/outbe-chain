use alloy_primitives::U256;
use outbe_primitives::{
    addresses::UPDATE_ADDRESS,
    error::{PrecompileError, Result},
    storage::StorageHandle,
};
use outbe_update::ProtocolVersion;

/// Resolves the Outbe protocol version from the exact execution state supplied
/// to the current top-level, nested or historical call.
///
/// The raw word is validated before conversion because `ProtocolVersion`'s
/// generic storage codec intentionally saturates and therefore cannot serve as
/// an activation authority for malformed consensus state.
pub(crate) fn resolve(storage: &StorageHandle<'_>) -> Result<ProtocolVersion> {
    let raw = storage.sload(UPDATE_ADDRESS, U256::ZERO)?;
    if raw > U256::from(u32::MAX) {
        return Err(PrecompileError::Fatal(
            "active Outbe protocol version does not fit the canonical u32 codec".into(),
        ));
    }
    Ok(ProtocolVersion::from_raw(raw.to::<u32>()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;

    #[test]
    fn resolver_reads_current_exact_state_and_rejects_noncanonical_width() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            assert_eq!(resolve(&storage).unwrap(), ProtocolVersion::ZERO);

            storage
                .sstore(UPDATE_ADDRESS, U256::ZERO, U256::from(2u64))
                .unwrap();
            assert_eq!(resolve(&storage).unwrap().raw(), 2);

            storage
                .sstore(
                    UPDATE_ADDRESS,
                    U256::ZERO,
                    U256::from(u32::MAX) + U256::from(1u64),
                )
                .unwrap();
            assert!(matches!(resolve(&storage), Err(PrecompileError::Fatal(_))));
        });
    }
}
