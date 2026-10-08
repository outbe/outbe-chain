use super::*;

pub(super) fn find_canonical_reward_gem_delivery_block_number(
    world: &World,
    gem_id: U256,
) -> Option<u64> {
    canonical_reward_gem_delivery_block_numbers(world, gem_id)
        .into_iter()
        .next()
}

pub(super) fn canonical_reward_gem_delivery_block_numbers(world: &World, gem_id: U256) -> Vec<u64> {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let Some(finalized) = world.rpc.finalized(port) else {
        return Vec::new();
    };
    let from = finalized.saturating_sub(MAX_REWARD_DELIVERY_SCAN_BLOCKS.saturating_sub(1));
    let Some(blocks) = eth::blocks_with_transactions(
        &url,
        from,
        finalized,
        usize::try_from(MAX_REWARD_DELIVERY_SCAN_BLOCKS).unwrap_or(usize::MAX),
    ) else {
        return Vec::new();
    };
    let gem_id_topic = format!("{:#066x}", gem_id);
    let gem_issued_topic = format!("{:#x}", eth::IGemFactory::GemIssued::SIGNATURE_HASH);
    let delivery_prefix = "0x4f53473202";
    let cycle_prefix = "0x4f53433202";
    let mut matching_block_numbers = Vec::new();
    for (block_number, block) in (from..=finalized).zip(blocks) {
        let Some(transactions) = block
            .get("transactions")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for (index, transaction) in transactions.iter().enumerate() {
            if index == 0 {
                continue;
            }
            let Some(transaction_hash) =
                transaction.get("hash").and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let Some(receipt) = eth::receipt_json(&url, transaction_hash) else {
                continue;
            };
            if transaction_is_reward_delivery_for_gem(
                &transactions[index - 1],
                transaction,
                &receipt,
                RewardDeliverySelectors {
                    cycle_prefix,
                    delivery_prefix,
                    gem_issued_topic: &gem_issued_topic,
                    gem_id_topic: &gem_id_topic,
                },
            ) {
                matching_block_numbers.push(block_number);
            }
        }
    }
    matching_block_numbers
}

pub(super) fn transaction_is_reward_delivery_for_gem(
    previous: &serde_json::Value,
    transaction: &serde_json::Value,
    receipt: &serde_json::Value,
    selectors: RewardDeliverySelectors<'_>,
) -> bool {
    let RewardDeliverySelectors {
        cycle_prefix,
        delivery_prefix,
        gem_issued_topic,
        gem_id_topic,
    } = selectors;
    let input = transaction
        .get("input")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let previous_input = previous
        .get("input")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let to = transaction
        .get("to")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<Address>().ok());
    let success = receipt
        .get("status")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|status| status == "0x1");
    let follows_cycle =
        input.starts_with(delivery_prefix) && previous_input.starts_with(cycle_prefix);
    let successful_system_delivery =
        to == Some(outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS) && success;
    follows_cycle
        && successful_system_delivery
        && receipt
            .get("logs")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|logs| {
                logs.iter().any(|log| {
                    let address_matches = log
                        .get("address")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|value| value.parse::<Address>().ok())
                        == Some(addresses::GEM_FACTORY_ADDR);
                    let topics = log.get("topics").and_then(serde_json::Value::as_array);
                    address_matches
                        && topics.is_some_and(|topics| {
                            topics
                                .first()
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|topic| topic.eq_ignore_ascii_case(gem_issued_topic))
                                && topics
                                    .get(1)
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(|topic| topic.eq_ignore_ascii_case(gem_id_topic))
                        })
                })
            })
}

pub(super) struct RewardDeliverySelectors<'a> {
    pub(super) cycle_prefix: &'a str,
    pub(super) delivery_prefix: &'a str,
    pub(super) gem_issued_topic: &'a str,
    pub(super) gem_id_topic: &'a str,
}
