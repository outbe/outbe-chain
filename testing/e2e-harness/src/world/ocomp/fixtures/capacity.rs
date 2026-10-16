//! Bulk capacity fixtures: Tribute owners, their balances, and L2 operators.

#[cfg(feature = "ocomp-integration")]
use crate::world::ocomp::*;
#[cfg(feature = "ocomp-integration")]
use outbe_l2registry::L2RegistryContract;
#[cfg(feature = "ocomp-integration")]
use outbe_primitives::addresses::L2_REGISTRY_ADDRESS;

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn capacity_tribute_private_keys(count: usize) -> Result<Vec<String>> {
    const FIRST_CAPACITY_SCALAR: u64 = 0x1_0000;

    let mut private_keys = Vec::new();
    private_keys.try_reserve_exact(count)?;
    for index in 0..count {
        let scalar = FIRST_CAPACITY_SCALAR
            .checked_add(u64::try_from(index)?)
            .ok_or_else(|| eyre::eyre!("capacity Tribute owner scalar overflow"))?;
        let mut bytes = [0_u8; 32];
        bytes[24..].copy_from_slice(&scalar.to_be_bytes());
        SigningKey::from_bytes((&bytes).into())
            .map_err(|error| eyre::eyre!("invalid capacity Tribute owner scalar: {error}"))?;
        private_keys.push(format!("0x{}", hex::encode(bytes)));
    }
    Ok(private_keys)
}

/// Seed explicitly listed bulk operators through the production registry API
/// before launch, avoiding a governance window per owner. Returns whether the
/// genesis changed. Existing registrations must match the requested fixture.
///
/// Each owner gets a fixture chain id and its deterministic
/// root-signing key, used to sign the offer's Merkle root.
#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn seed_capacity_operator_l2_registrations(
    genesis: &mut serde_json::Value,
    chain_id: u64,
    private_keys: &[String],
) -> Result<bool> {
    if private_keys.is_empty() {
        return Ok(false);
    }
    let registrations = capacity_l2_registrations(private_keys)?;
    let mut provider = l2_registry_provider(genesis, chain_id)?;
    provider.set_block_number(1);
    let changed = StorageHandle::enter(&mut provider, |storage| {
        register_capacity_operators(storage, &registrations)
    })?;
    if !changed {
        return Ok(false);
    }
    write_l2_registry_storage(genesis, &provider)?;
    Ok(true)
}

/// One bulk operator: its fixture chain id, L1 owner, and root-signing key.
#[cfg(feature = "ocomp-integration")]
type CapacityL2Registration = (u64, Address, Vec<u8>);

#[cfg(feature = "ocomp-integration")]
fn capacity_l2_registrations(private_keys: &[String]) -> Result<Vec<CapacityL2Registration>> {
    let mut registrations = Vec::with_capacity(private_keys.len());
    for (index, private_key) in private_keys.iter().enumerate() {
        let l1_address = crate::internal::eth::address_of(private_key)
            .ok_or_else(|| eyre::eyre!("cannot derive bulk Tribute owner"))?;
        let operator_chain_id = CAPACITY_OPERATOR_L2_CHAIN_ID_BASE
            .checked_add(u64::try_from(index)?)
            .ok_or_else(|| eyre::eyre!("bulk L2 chain id overflow"))?;
        u32::try_from(operator_chain_id)
            .map_err(|_| eyre::eyre!("bulk L2 chain id exceeds the offer selector width"))?;
        registrations.push((
            operator_chain_id,
            l1_address,
            crate::internal::l2_fixture::root_signing_public_key(operator_chain_id),
        ));
    }
    Ok(registrations)
}

#[cfg(feature = "ocomp-integration")]
fn l2_registry_provider(
    genesis: &serde_json::Value,
    chain_id: u64,
) -> Result<HashMapStorageProvider> {
    let mut provider = HashMapStorageProvider::new(chain_id);
    let alloc = genesis
        .get("alloc")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    let existing = find_alloc_address_key(alloc, L2_REGISTRY_ADDRESS)?
        .and_then(|key| alloc.get(&key))
        .and_then(|account| account.get("storage"))
        .and_then(serde_json::Value::as_object);
    for (slot, value) in existing.into_iter().flatten() {
        provider.storage.insert(
            (L2_REGISTRY_ADDRESS, parse_hex_word(slot)?),
            parse_storage_word(value)?,
        );
    }
    Ok(provider)
}

#[cfg(feature = "ocomp-integration")]
fn register_capacity_operators(
    storage: StorageHandle<'_>,
    registrations: &[CapacityL2Registration],
) -> Result<bool> {
    let mut registry = L2RegistryContract::new(storage);
    let mut changed = false;
    for (operator_chain_id, l1_address, public_key) in registrations {
        if let Some(record) = registry.networks.get(*operator_chain_id)? {
            eyre::ensure!(
                record.l1_address == *l1_address
                    && record.public_key_bytes()?.as_slice() == public_key.as_slice()
                    && registry.l1_to_chain.read(l1_address)? == *operator_chain_id,
                "conflicting bulk L2 registration for chain {operator_chain_id}"
            );
        } else {
            registry.register_network(*operator_chain_id, *l1_address, public_key)?;
            changed = true;
        }
    }
    Ok(changed)
}

#[cfg(feature = "ocomp-integration")]
fn write_l2_registry_storage(
    genesis: &mut serde_json::Value,
    provider: &HashMapStorageProvider,
) -> Result<()> {
    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    let account_key = match find_alloc_address_key(alloc, L2_REGISTRY_ADDRESS)? {
        Some(key) => key,
        None => {
            let key = format!("{L2_REGISTRY_ADDRESS:x}");
            alloc.insert(
                key.clone(),
                serde_json::json!({ "balance": "0x0", "code": "0xef", "storage": {} }),
            );
            key
        }
    };
    let words = alloc
        .get_mut(&account_key)
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("L2Registry genesis account is not an object"))?
        .entry("storage".to_owned())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| eyre::eyre!("L2Registry genesis account storage is not an object"))?;
    for ((address, slot), value) in &provider.storage {
        if *address != L2_REGISTRY_ADDRESS || value.is_zero() {
            continue;
        }
        words.insert(
            format!("0x{slot:064x}"),
            serde_json::Value::String(format!("0x{value:064x}")),
        );
    }
    Ok(())
}

/// Bulk fixture ids use a separate namespace from governed operators.
#[cfg(feature = "ocomp-integration")]
const CAPACITY_OPERATOR_L2_CHAIN_ID_BASE: u64 = 0xE2E1_0000;

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn fund_capacity_tribute_accounts(
    genesis: &mut serde_json::Value,
    private_keys: &[String],
) -> Result<bool> {
    const CAPACITY_OWNER_BALANCE_COEN: u64 = 1_000;
    const COEN_BASE_UNITS: u64 = 1_000_000_000_000_000_000;

    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    let balance = U256::from(CAPACITY_OWNER_BALANCE_COEN)
        .checked_mul(U256::from(COEN_BASE_UNITS))
        .ok_or_else(|| eyre::eyre!("capacity Tribute owner balance overflow"))?;
    let balance_hex = format!("{balance:#x}");
    let mut changed = false;
    for private_key in private_keys {
        let owner = crate::internal::eth::address_of(private_key)
            .ok_or_else(|| eyre::eyre!("cannot derive capacity Tribute owner"))?;
        let key = format!("{owner:#x}");
        match alloc.get(&key) {
            Some(existing)
                if existing.as_object().is_some_and(|account| {
                    account.len() == 1
                        && account.get("balance").and_then(serde_json::Value::as_str)
                            == Some(balance_hex.as_str())
                }) => {}
            Some(_) => {
                eyre::bail!(
                    "capacity Tribute owner {owner:#x} collides with a different genesis account"
                );
            }
            None => {
                alloc.insert(
                    key,
                    serde_json::json!({
                        "balance": balance_hex,
                    }),
                );
                changed = true;
            }
        }
    }
    Ok(changed)
}

#[cfg(all(test, feature = "ocomp-integration"))]
mod l2_registration_tests {
    use super::*;

    #[test]
    fn bulk_l2_genesis_seeding_is_idempotent_and_rejects_conflicts() {
        let owner = format!("{:#x}", Address::repeat_byte(0x77));
        let account = serde_json::json!({ "balance": "0x1234" });
        let mut genesis = serde_json::json!({ "alloc": { owner.clone(): account.clone() } });
        let keys = [format!("{:064x}", 1), format!("{:064x}", 2)];
        let chain_id = outbe_primitives::chain::DEVNET_CHAIN_ID;

        assert!(seed_capacity_operator_l2_registrations(&mut genesis, chain_id, &keys).unwrap());
        assert_eq!(genesis["alloc"][&owner], account);
        let seeded = genesis.clone();
        assert!(!seed_capacity_operator_l2_registrations(&mut genesis, chain_id, &keys).unwrap());
        assert_eq!(genesis, seeded);

        let conflicting = [keys[1].clone(), keys[0].clone()];
        assert!(
            seed_capacity_operator_l2_registrations(&mut genesis, chain_id, &conflicting).is_err()
        );
        assert_eq!(genesis, seeded);
    }
}
