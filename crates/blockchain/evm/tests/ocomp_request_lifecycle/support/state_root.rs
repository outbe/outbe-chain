use super::super::*;

pub(in crate::lifecycle) type HashedAccountState = BTreeMap<B256, (Account, BTreeMap<B256, U256>)>;

pub(in crate::lifecycle) fn hashed_marker_state(
    storage: &HashMap<(Address, U256), U256>,
) -> HashedAccountState {
    let marker_code_hash = keccak256([0xef]);
    let mut accounts = HashedAccountState::new();
    for validator_index in 0_u8..4 {
        accounts.insert(
            keccak256(validator_sender(validator_index)),
            (
                Account {
                    nonce: 0,
                    balance: vote_sender_balance(),
                    bytecode_hash: None,
                },
                BTreeMap::new(),
            ),
        );
    }
    accounts.insert(
        keccak256(saturated_user_sender()),
        (
            Account {
                nonce: 0,
                balance: vote_sender_balance(),
                bytecode_hash: None,
            },
            BTreeMap::new(),
        ),
    );
    accounts.insert(
        keccak256(BURNER_ADDRESS),
        (
            Account {
                nonce: 0,
                balance: U256::ZERO,
                bytecode_hash: Some(keccak256([0x5b, 0x60, 0x00, 0x56])),
            },
            BTreeMap::new(),
        ),
    );
    for ((address, slot), value) in storage {
        let (_, account_storage) = accounts.entry(keccak256(address)).or_insert_with(|| {
            (
                Account {
                    nonce: 0,
                    balance: U256::ZERO,
                    bytecode_hash: Some(marker_code_hash),
                },
                BTreeMap::new(),
            )
        });
        if !value.is_zero() {
            account_storage.insert(keccak256(B256::from(slot.to_be_bytes::<32>())), *value);
        }
    }
    accounts
}

pub(in crate::lifecycle) fn apply_storage_overlay(
    storage: &mut BTreeMap<B256, U256>,
    post_state: HashedStorage,
) {
    for (slot, value) in post_state.storage {
        if value.is_zero() {
            storage.remove(&slot);
        } else {
            storage.insert(slot, value);
        }
    }
}

pub(in crate::lifecycle) fn state_root_with_overlay(
    base_state: &HashedAccountState,
    post_state: HashedPostState,
) -> B256 {
    let mut state = base_state.clone();
    let destroyed_accounts = post_state
        .accounts
        .iter()
        .filter_map(|(address, account)| account.is_none().then_some(*address))
        .collect::<std::collections::BTreeSet<_>>();
    for (address, account) in post_state.accounts {
        match account {
            Some(account) => {
                let storage = state
                    .remove(&address)
                    .map(|(_, storage)| storage)
                    .unwrap_or_default();
                state.insert(address, (account, storage));
            }
            None => {
                state.remove(&address);
            }
        }
    }
    for (address, storage_overlay) in post_state.storages {
        let Some((_, storage)) = state.get_mut(&address) else {
            assert!(
                destroyed_accounts.contains(&address)
                    && storage_overlay.storage.values().all(U256::is_zero),
                "post-state storage exists without a live account"
            );
            continue;
        };
        apply_storage_overlay(storage, storage_overlay);
    }
    state_root_prehashed(state)
}

pub(in crate::lifecycle) fn mutate_one_storage_value(
    mut post_state: HashedPostState,
) -> HashedPostState {
    let value = post_state
        .storages
        .values_mut()
        .flat_map(|storage| storage.storage.values_mut())
        .next()
        .expect("request execution changes at least one storage value");
    *value = if *value == U256::MAX {
        value.checked_sub(U256::from(1)).unwrap()
    } else {
        value.checked_add(U256::from(1)).unwrap()
    };
    post_state
}

pub(in crate::lifecycle) fn apply_bundle(
    target: &mut HashMapStorageProvider,
    state: &revm::primitives::AddressMap<revm::database::states::BundleAccount>,
) {
    for (address, account) in state {
        for (slot, value) in &account.storage {
            target
                .storage
                .insert((*address, *slot), value.present_value());
        }
    }
}
