//! Source resolution and state materialization stay inside the enclave. The
//! cache advances only from authenticated journal records supplied by execution.
use crate::{fidelity, gratis};
use alloy_primitives::{Address, B256, U256};
use outbe_tee::{confidential::*, protocol::*};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Allocation {
    credis_id: U256,
    source: Address,
    remaining: U256,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Account {
    balance: Vec<u8>,
    pledged: Vec<u8>,
    nonce: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Delta {
    Gratis {
        account: Address,
        state: Account,
        ticket: Option<(B256, Vec<u8>)>,
        allocation: Option<(B256, Option<Allocation>)>,
    },
    // An accepted LIFO cohort change, with its original timestamp. Materializing
    // it has no money movement, authorization, events, or public effects.
    Fidelity {
        account: Address,
        amount: U256,
        op: FidelityCohortOp,
        timestamp: u64,
        anchor: u64,
    },
}
#[derive(Default)]
struct Cache {
    head: Head,
    accounts: BTreeMap<Address, Account>,
    tickets: BTreeMap<B256, Vec<u8>>,
    allocations: BTreeMap<B256, Allocation>,
    closed: BTreeSet<B256>,
    cohorts: BTreeMap<Address, Vec<u8>>,
    recent: VecDeque<(Head, Undo)>,
}
// A bounded undo cache keeps ordinary transaction rollback independent of total
// journal length. Older historical states recover from the chain journal.
// At most 64 inline entries per domain; avoid a heap allocation for every write.
#[allow(clippy::large_enum_variant)]
enum Undo {
    Gratis {
        account: Address,
        state: Option<Account>,
        ticket: Option<(B256, Option<Vec<u8>>)>,
        allocation: Option<(B256, Option<Allocation>, bool)>,
    },
    Fidelity {
        account: Address,
        state: Option<Vec<u8>>,
    },
}
fn remember(cache: &Cache, delta: &Delta) -> Undo {
    match delta {
        Delta::Gratis {
            account,
            ticket,
            allocation,
            ..
        } => Undo::Gratis {
            account: *account,
            state: cache.accounts.get(account).cloned(),
            ticket: ticket
                .as_ref()
                .map(|(id, _)| (*id, cache.tickets.get(id).cloned())),
            allocation: allocation.as_ref().map(|(id, _)| {
                (
                    *id,
                    cache.allocations.get(id).cloned(),
                    cache.closed.contains(id),
                )
            }),
        },
        Delta::Fidelity { account, .. } => Undo::Fidelity {
            account: *account,
            state: cache.cohorts.get(account).cloned(),
        },
    }
}
fn restore<K: Ord, V>(map: &mut BTreeMap<K, V>, key: K, value: Option<V>) {
    if let Some(value) = value {
        map.insert(key, value);
    } else {
        map.remove(&key);
    }
}
fn rewind(cache: &mut Cache, target: Head) {
    while cache.head.count >= target.count && cache.head != target {
        let Some((head, undo)) = cache.recent.pop_back() else {
            *cache = Cache::default();
            break;
        };
        match undo {
            Undo::Gratis {
                account,
                state,
                ticket,
                allocation,
            } => {
                restore(&mut cache.accounts, account, state);
                if let Some((id, ticket)) = ticket {
                    restore(&mut cache.tickets, id, ticket);
                }
                if let Some((id, allocation, closed)) = allocation {
                    restore(&mut cache.allocations, id, allocation);
                    if closed {
                        cache.closed.insert(id);
                    } else {
                        cache.closed.remove(&id);
                    }
                }
            }
            Undo::Fidelity { account, state } => restore(&mut cache.cohorts, account, state),
        }
        cache.head = head;
    }
}
type CacheKey = (B256, B256, u8);
thread_local! { static CACHES: RefCell<BTreeMap<CacheKey, Cache>> = const { RefCell::new(BTreeMap::new()) }; }
fn cache_key(key: &[u8; 32], chain: B256, domain: Domain) -> CacheKey {
    (alloy_primitives::keccak256(key), chain, domain as u8)
}
fn context(chain: B256, domain: Domain, head: Head) -> Vec<u8> {
    let mut bytes = b"outbe/confidential/journal/v1".to_vec();
    bytes.extend_from_slice(chain.as_slice());
    bytes.push(domain as u8);
    bytes.extend_from_slice(&head.count.to_be_bytes());
    bytes.extend_from_slice(head.hash.as_slice());
    bytes
}
fn encode_delta(
    key: &[u8; 32],
    chain: B256,
    domain: Domain,
    before: Head,
    request_hash: B256,
    delta: &Delta,
) -> Result<Update, String> {
    let bytes = serde_json::to_vec(&(request_hash, delta)).map_err(|e| e.to_string())?;
    // JSON length is bounded by the fixed padded record, therefore fits u32.
    let len = u32::try_from(bytes.len()).map_err(|_| "delta too large")?;
    let mut plain = len.to_be_bytes().to_vec();
    plain.extend(bytes);
    let record = seal(
        key,
        &context(chain, domain, before),
        &plain,
        RECORD_BYTES - 48,
    )?;
    Ok(Update {
        domain,
        before,
        record,
    })
}
fn decode_delta(
    key: &[u8; 32],
    chain: B256,
    domain: Domain,
    before: Head,
    record: &[u8],
) -> Result<Delta, String> {
    if record.len() != RECORD_BYTES {
        return Err("invalid journal record size".into());
    }
    let plain = open(key, &context(chain, domain, before), record)?;
    let prefix: [u8; 4] = plain
        .get(..4)
        .ok_or("invalid record")?
        .try_into()
        .map_err(|_| "invalid record")?;
    let len = u32::from_be_bytes(prefix) as usize;
    serde_json::from_slice::<(B256, Delta)>(
        plain
            .get(4..4usize.checked_add(len).ok_or("invalid record length")?)
            .ok_or("invalid record length")?,
    )
    .map(|(_, delta)| delta)
    .map_err(|e| e.to_string())
}
fn materialize(
    cache: &mut Cache,
    key: &[u8; 32],
    domain: Domain,
    delta: Delta,
) -> Result<(), String> {
    match (domain, delta) {
        (
            Domain::Gratis,
            Delta::Gratis {
                account,
                state,
                ticket,
                allocation,
            },
        ) => {
            cache.accounts.insert(account, state);
            if let Some((id, bytes)) = ticket {
                if bytes.is_empty() {
                    cache.tickets.remove(&id);
                } else {
                    cache.tickets.insert(id, bytes);
                }
            }
            if let Some((id, allocation)) = allocation {
                match allocation {
                    Some(a) => {
                        if cache.closed.contains(&id) {
                            return Err("allocation already closed".into());
                        }
                        cache.allocations.insert(id, a);
                    }
                    None => {
                        cache.allocations.remove(&id);
                        cache.closed.insert(id);
                    }
                }
            }
        }
        (
            Domain::Fidelity,
            Delta::Fidelity {
                account,
                amount,
                op,
                timestamp,
                anchor,
            },
        ) => {
            let section = FidelityOpSection {
                op,
                timestamp,
                first_qualified_start: anchor,
                current_blob: cache.cohorts.get(&account).cloned().unwrap_or_default(),
            };
            let outcome = fidelity::apply_cohort_section(key, account, amount, &section)
                .map_err(|e| e.to_string())?;
            cache.cohorts.insert(account, outcome.new_blob);
        }
        _ => return Err("journal domain mismatch".into()),
    }
    Ok(())
}

pub fn dispatch(
    wire: &EnclaveRequest,
    gratis_key: &[u8; 32],
    fidelity_key: &[u8; 32],
    offer_secret: &[u8; 32],
) -> Response {
    match execute(wire, gratis_key, fidelity_key, offer_secret) {
        Ok(response) => response,
        Err(reason) => Response::Rejected { reason },
    }
}
fn execute(
    wire: &EnclaveRequest,
    gkey: &[u8; 32],
    fkey: &[u8; 32],
    offer_secret: &[u8; 32],
) -> Result<Response, String> {
    CACHES.with(|caches| {
        let mut caches = caches
            .try_borrow_mut()
            .map_err(|_| "confidential cache reentry")?;
        if let EnclaveRequest::LoadConfidential { page } = wire {
            if page.records.is_empty() || page.records.len() > PAGE_RECORDS as usize {
                return Err("invalid recovery page".into());
            }
            let key = match page.domain {
                Domain::Gratis => gkey,
                Domain::Fidelity => fkey,
            };
            let cache = caches
                .entry(cache_key(key, page.chain_id, page.domain))
                .or_default();
            let mut end = page.after;
            for record in &page.records {
                end = advance(end, record).ok_or("journal exhausted")?;
            }
            if cache.head == end {
                return Ok(Response::Loaded);
            }
            if page.after == Head::default() {
                *cache = Cache::default();
            }
            if cache.head != page.after {
                return Ok(Response::Missing {
                    domain: page.domain,
                    after: cache.head,
                });
            }
            let loaded = (|| {
                for record in &page.records {
                    let delta = decode_delta(key, page.chain_id, page.domain, cache.head, record)?;
                    let undo = remember(cache, &delta);
                    materialize(cache, key, page.domain, delta)?;
                    cache.recent.push_back((cache.head, undo));
                    if cache.recent.len() > 64 {
                        cache.recent.pop_front();
                    }
                    cache.head = advance(cache.head, record).ok_or("journal exhausted")?;
                }
                Ok(Response::Loaded)
            })();
            if loaded.is_err() {
                *cache = Cache::default();
            }
            return loaded;
        }
        let EnclaveRequest::Confidential { request: req } = wire else {
            return Err("invalid confidential request".into());
        };
        for (domain, key, target) in [
            (Domain::Gratis, gkey, req.gratis),
            (Domain::Fidelity, fkey, req.fidelity),
        ] {
            let cache = caches
                .entry(cache_key(key, req.chain_id, domain))
                .or_default();
            if cache.head != target {
                if cache.head.count >= target.count {
                    rewind(cache, target);
                }
                if cache.head != target {
                    return Ok(Response::Missing {
                        domain,
                        after: cache.head,
                    });
                }
            }
        }
        let g = caches
            .get(&cache_key(gkey, req.chain_id, Domain::Gratis))
            .ok_or("missing Gratis cache")?;
        let f = caches
            .get(&cache_key(fkey, req.chain_id, Domain::Fidelity))
            .ok_or("missing Fidelity cache")?;
        let request_hash = hash(req).map_err(|e| e.to_string())?;
        let mut updates = Vec::new();
        let value = match &req.call {
            Call::Gratis(input) => {
                if !matches!(
                    input.op,
                    GratisOp::Mint | GratisOp::Burn | GratisOp::Pledge | GratisOp::Unpledge
                ) {
                    return Err("unsupported account operation".into());
                }
                let mut input = *input.clone();
                if !input.current_balance.is_empty()
                    || !input.current_pledged.is_empty()
                    || !input.current_pledge_record.is_empty()
                {
                    return Err("host supplied account state".into());
                }
                if let Some(section) = &input.fidelity {
                    if !section.current_blob.is_empty() {
                        return Err("fidelity section must use journal state".into());
                    }
                    if !matches!(
                        (input.op, section.op),
                        (GratisOp::Mint, FidelityCohortOp::In)
                            | (GratisOp::Burn, FidelityCohortOp::Out)
                            | (GratisOp::Pledge, FidelityCohortOp::Probe)
                    ) {
                        return Err("invalid Fidelity action".into());
                    }
                }
                if input.chain_id != req.chain_id {
                    return Err("chain mismatch".into());
                }
                let account = g.accounts.get(&input.account).cloned().unwrap_or_default();
                if input.modify_auth.op_nonce != account.nonce {
                    return Err("invalid op nonce".into());
                }
                input.current_balance = account.balance.clone();
                input.current_pledged = account.pledged.clone();
                input.current_pledge_record = input
                    .pledge_note
                    .and_then(|n| g.tickets.get(&n).cloned())
                    .unwrap_or_default();
                let mut result = gratis::apply_op(gkey, &input);
                applied(&result)?;
                let ticket = match input.op {
                    GratisOp::Pledge => {
                        Some((result.pledge_note, result.new_pledge_record.clone()))
                    }
                    GratisOp::Unpledge => {
                        Some((input.pledge_note.ok_or("missing note")?, Vec::new()))
                    }
                    _ => None,
                };
                let state = Account {
                    balance: choose(&result.new_balance, &account.balance),
                    pledged: choose(&result.new_pledged, &account.pledged),
                    nonce: result.next_op_nonce,
                };
                updates.push(encode_delta(
                    gkey,
                    req.chain_id,
                    Domain::Gratis,
                    req.gratis,
                    request_hash,
                    &Delta::Gratis {
                        account: input.account,
                        state,
                        ticket,
                        allocation: None,
                    },
                )?);
                if let Some(section) = &input.fidelity {
                    result.fidelity = Some(cohort(
                        f,
                        fkey,
                        req,
                        input.account,
                        input.amount,
                        section,
                        &mut updates,
                    )?);
                }
                // The note is only delivered encrypted to its owner; the public
                // issuance credential uses fresh ECDH encryption.
                if input.op == GratisOp::Pledge {
                    let view =
                        gratis::derive_view_key(gkey, input.account).map_err(|e| e.to_string())?;
                    result.new_pledge_record = seal(
                        &view,
                        b"outbe/pledge-reply/v1",
                        result.pledge_note.as_slice(),
                        32,
                    )?;
                } else {
                    result.new_pledge_record.clear();
                }
                result.pledge_note = B256::ZERO;
                result.new_balance.clear();
                result.new_pledged.clear();
                Value::Gratis(Box::new(result))
            }
            Call::Activate {
                credis_id,
                smart_account,
                credential,
                timestamp,
            } => {
                let encrypted = crate::crypto::EncryptedShare::from_bytes(credential)
                    .map_err(|e| e.to_string())?;
                let plain = crate::crypto::decrypt_share(offer_secret, &encrypted)
                    .map_err(|e| e.to_string())?;
                // Domain + network + note + destination + authorization, fixed length.
                if plain.len() != 148
                    || &plain[..32] != b"outbe/credis/credential/v1\0\0\0\0\0\0"
                    || &plain[32..64] != req.chain_id.as_slice()
                    || &plain[96..116] != smart_account.as_slice()
                {
                    return Err("invalid pledge credential".into());
                }
                let note = B256::from_slice(&plain[64..96]);
                let spend: [u8; 32] = plain[116..148]
                    .try_into()
                    .map_err(|_| "invalid spend authorization")?;
                let ticket = g.tickets.get(&note).ok_or("unknown or consumed pledge")?;
                let source = gratis::ticket_owner(gkey, note, ticket).map_err(|e| e.to_string())?;
                let collateral_id = B256::from(
                    crate::crypto::hkdf_sha256(
                        gkey,
                        &credis_id.to_be_bytes::<32>(),
                        b"outbe/collateral-id/v1",
                    )
                    .map_err(|e| e.to_string())?,
                );
                if g.allocations.contains_key(&collateral_id) || g.closed.contains(&collateral_id) {
                    return Err("allocation already exists".into());
                }
                let account = g.accounts.get(&source).cloned().unwrap_or_default();
                let mut input =
                    empty_request(GratisOp::ConsumePledge, req.chain_id, source, U256::ZERO);
                input.block_timestamp = *timestamp;
                input.current_pledged = account.pledged.clone();
                input.current_pledge_record = ticket.clone();
                input.pledge_note = Some(note);
                input.smart_account = Some(*smart_account);
                input.spend_auth = Some(spend);
                let result = gratis::apply_op(gkey, &input);
                applied(&result)?;
                let terms = result.pledge_terms.ok_or("missing pledge terms")?;
                let state = Account {
                    pledged: result.new_pledged,
                    ..account
                };
                let allocation = Allocation {
                    credis_id: *credis_id,
                    source,
                    remaining: terms.gratis_amount,
                };
                updates.push(encode_delta(
                    gkey,
                    req.chain_id,
                    Domain::Gratis,
                    req.gratis,
                    request_hash,
                    &Delta::Gratis {
                        account: source,
                        state,
                        ticket: Some((note, Vec::new())),
                        allocation: Some((collateral_id, Some(allocation))),
                    },
                )?);
                Value::Activated {
                    terms,
                    collateral_id,
                }
            }
            Call::Collateral {
                authorization: a,
                timestamp,
                fidelity_anchor,
            } => {
                let mut allocation = g
                    .allocations
                    .get(&a.collateral_id)
                    .cloned()
                    .ok_or("unknown or closed allocation")?;
                if allocation.credis_id != a.credis_id {
                    return Err("collateral Credis binding mismatch".into());
                }
                if allocation.remaining != a.expected_remaining {
                    return Err("stale collateral authorization".into());
                }
                if a.amount.is_zero() || a.amount > allocation.remaining {
                    return Err("collateral allocation exceeded".into());
                }
                let source = allocation.source;
                let account = g
                    .accounts
                    .get(&source)
                    .cloned()
                    .ok_or("missing source account")?;
                let op = match a.action {
                    CollateralAction::Return => GratisOp::ReleaseToEoa,
                    CollateralAction::Burn => GratisOp::BurnPledged,
                };
                let mut input = empty_request(op, req.chain_id, source, a.amount);
                input.current_balance = account.balance.clone();
                input.current_pledged = account.pledged.clone();
                let result = gratis::apply_op(gkey, &input);
                applied(&result)?;
                allocation.remaining = allocation
                    .remaining
                    .checked_sub(a.amount)
                    .ok_or("allocation underflow")?;
                let state = Account {
                    balance: choose(&result.new_balance, &account.balance),
                    pledged: choose(&result.new_pledged, &account.pledged),
                    ..account
                };
                let remaining = if allocation.remaining.is_zero() {
                    None
                } else {
                    Some(allocation)
                };
                updates.push(encode_delta(
                    gkey,
                    req.chain_id,
                    Domain::Gratis,
                    req.gratis,
                    request_hash,
                    &Delta::Gratis {
                        account: source,
                        state,
                        ticket: None,
                        allocation: Some((a.collateral_id, remaining)),
                    },
                )?);
                if a.action == CollateralAction::Burn {
                    let section = FidelityOpSection {
                        op: FidelityCohortOp::Out,
                        timestamp: *timestamp,
                        first_qualified_start: *fidelity_anchor,
                        current_blob: Vec::new(),
                    };
                    cohort(f, fkey, req, source, a.amount, &section, &mut updates)?;
                }
                Value::Collateral {
                    amount: result.gratis_amount,
                }
            }
            Call::GratisView { account, field } => {
                let state = g.accounts.get(account).cloned().unwrap_or_default();
                let key = gratis::derive_view_key(gkey, *account).map_err(|e| e.to_string())?;
                let amount = match field {
                    0 => gratis::decrypt_balance(&key, *account, &state.balance),
                    1 => gratis::decrypt_pledged(&key, *account, &state.pledged),
                    2 => Ok(U256::ZERO),
                    _ => return Err("invalid view field".into()),
                }
                .map_err(|e| e.to_string())?;
                let mut context = req.gratis.hash.as_slice().to_vec();
                context.extend_from_slice(account.as_slice());
                context.push(*field);
                let mut blob = req.gratis.hash.as_slice().to_vec();
                blob.extend(seal(&key, &context, &amount.to_be_bytes::<32>(), 32)?);
                Value::View {
                    blob,
                    nonce: state.nonce,
                }
            }
            Call::Fidelity(input) => Value::Fidelity(cohort(
                f,
                fkey,
                req,
                input.account,
                input.amount,
                &input.section,
                &mut updates,
            )?),
            Call::FidelitySnapshot(input) => {
                let mut input = *input.clone();
                for entry in &mut input.entries {
                    entry.cohort_blob = f.cohorts.get(&entry.owner).cloned().unwrap_or_default();
                }
                Value::Snapshot(
                    fidelity::snapshot_leagues(fkey, &input).map_err(|e| e.to_string())?,
                )
            }
            Call::FidelityQuery(input) => {
                let mut input = *input.clone();
                input.cohort_blob = f.cohorts.get(&input.account).cloned().unwrap_or_default();
                Value::Query(
                    fidelity::query_index(fkey, req.chain_id, &input).map_err(|e| e.to_string())?,
                )
            }
        };
        Ok(Response::Applied(Box::new(Applied {
            request_hash,
            value,
            updates,
            attestation: Vec::new(),
        })))
    })
}
fn choose(new: &[u8], old: &[u8]) -> Vec<u8> {
    if new.is_empty() {
        old.to_vec()
    } else {
        new.to_vec()
    }
}
fn applied(result: &GratisOpResult) -> Result<(), String> {
    match &result.status {
        GratisOpStatus::Applied => Ok(()),
        GratisOpStatus::Rejected { reason } => Err(reason.clone()),
    }
}
fn cohort(
    cache: &Cache,
    key: &[u8; 32],
    req: &Request,
    account: Address,
    amount: U256,
    section: &FidelityOpSection,
    updates: &mut Vec<Update>,
) -> Result<FidelityOpOutcome, String> {
    let mut section = section.clone();
    section.current_blob = cache.cohorts.get(&account).cloned().unwrap_or_default();
    let mut result = fidelity::apply_cohort_section(key, account, amount, &section)
        .map_err(|e| e.to_string())?;
    if section.op != FidelityCohortOp::Probe {
        updates.push(encode_delta(
            key,
            req.chain_id,
            Domain::Fidelity,
            req.fidelity,
            hash(req).map_err(|e| e.to_string())?,
            &Delta::Fidelity {
                account,
                amount,
                op: section.op,
                timestamp: section.timestamp,
                anchor: section.first_qualified_start,
            },
        )?);
    }
    result.new_blob.clear();
    Ok(result)
}
pub fn empty_request(
    op: GratisOp,
    chain_id: B256,
    account: Address,
    amount: U256,
) -> GratisOpRequest {
    GratisOpRequest {
        op,
        chain_id,
        block_timestamp: 0,
        account,
        amount,
        current_balance: Vec::new(),
        current_pledged: Vec::new(),
        current_pledge_record: Vec::new(),
        modify_auth: ModifyAuth {
            mac: [0; 32],
            op_nonce: 0,
        },
        pledge_note: None,
        smart_account: None,
        spend_auth: None,
        pledge_terms: None,
        fidelity: None,
    }
}

/// Drop disposable materialization, for restart/recovery checks and key rotation.
pub fn clear_cache() {
    CACHES.with(|caches| caches.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn recovery_retries_restart_and_authenticate_pages() {
        use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

        clear_cache();
        let chain = B256::from(U256::ONE);
        let gkey = [0x31; 32];
        let fkey = [0x32; 32];
        let account = Address::repeat_byte(0x33);
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let mut updates = Vec::new();
            let mut cursor = Head::default();
            for nonce in 1..=PAGE_RECORDS + 1 {
                let update = encode_delta(
                    &gkey,
                    chain,
                    Domain::Gratis,
                    cursor,
                    B256::ZERO,
                    &Delta::Gratis {
                        account,
                        state: Account {
                            nonce,
                            ..Account::default()
                        },
                        ticket: None,
                        allocation: None,
                    },
                )
                .unwrap();
                outbe_tee::confidential::persist(&storage, &update).unwrap();
                cursor = advance(cursor, &update.record).unwrap();
                updates.push(update);
            }
            let mut restarted = false;
            let applied = outbe_tee::confidential::execute(
                &storage,
                Call::GratisView { account, field: 2 },
                |wire| {
                    // Lose the cache between pages, as a reconnect to a new enclave
                    // connection can do. Duplicate delivery must also be harmless.
                    if let EnclaveRequest::LoadConfidential { page } = wire {
                        if page.after.count > 0 && !restarted {
                            clear_cache();
                            restarted = true;
                        }
                    }
                    let response = dispatch(wire, &gkey, &fkey, &[0; 32]);
                    if response == Response::Loaded {
                        assert_eq!(dispatch(wire, &gkey, &fkey, &[0; 32]), Response::Loaded);
                    }
                    Some(EnclaveResponse::Confidential { response })
                },
            )
            .unwrap();
            assert!(restarted);
            assert!(
                matches!(applied.value, Value::View { nonce, .. } if nonce == PAGE_RECORDS + 1)
            );

            // Authentication covers the chain, domain and preceding head, and
            // a rejected page must not leave partially materialized state.
            for invalid in 0..4 {
                let mut page = Page {
                    chain_id: chain,
                    domain: Domain::Gratis,
                    after: Head::default(),
                    records: vec![updates[0].record.clone(), updates[1].record.clone()],
                };
                match invalid {
                    0 => page.records[1][100] ^= 1,
                    1 => page.records.swap(0, 1),
                    2 => page.domain = Domain::Fidelity,
                    _ => page.chain_id = B256::repeat_byte(0x34),
                }
                assert!(matches!(
                    dispatch(
                        &EnclaveRequest::LoadConfidential { page },
                        &gkey,
                        &fkey,
                        &[0; 32]
                    ),
                    Response::Rejected { .. }
                ));
            }
            let page = Page {
                chain_id: chain,
                domain: Domain::Gratis,
                after: Head::default(),
                records: updates
                    .into_iter()
                    .take(PAGE_RECORDS as usize)
                    .map(|u| u.record)
                    .collect(),
            };
            assert_eq!(
                dispatch(
                    &EnclaveRequest::LoadConfidential { page },
                    &gkey,
                    &fkey,
                    &[0; 32]
                ),
                Response::Loaded
            );
        });
    }

    /// Measures source lookup/sealing separately from EVM storage and SGX entry.
    #[test]
    #[ignore = "explicit confidential-journal scaling measurement"]
    fn journal_scaling() {
        let chain = B256::repeat_byte(0x65);
        let gkey = [0x61; 32];
        let fkey = [0x62; 32];
        for population in [1u64, 1000, 10000] {
            clear_cache();
            let mut records = Vec::new();
            let mut head = Head::default();
            for i in 1..=population {
                let source = Address::from_word(B256::from(U256::from(i)));
                let view = gratis::derive_view_key(&gkey, source).unwrap();
                let state = Account {
                    pledged: crate::confidential::GRATIS
                        .write_amount(&view, source, 1, 0, U256::from(100))
                        .unwrap(),
                    ..Account::default()
                };
                let delta = Delta::Gratis {
                    account: source,
                    state,
                    ticket: None,
                    allocation: Some((
                        B256::from(U256::from(i)),
                        Some(Allocation {
                            credis_id: U256::from(i),
                            source,
                            remaining: U256::from(100),
                        }),
                    )),
                };
                let update =
                    encode_delta(&gkey, chain, Domain::Gratis, head, B256::ZERO, &delta).unwrap();
                head = advance(head, &update.record).unwrap();
                records.push(update.record);
            }
            let recovery = Instant::now();
            let mut cursor = Head::default();
            for page in records.chunks(PAGE_RECORDS as usize) {
                let wire = EnclaveRequest::LoadConfidential {
                    page: Page {
                        chain_id: chain,
                        domain: Domain::Gratis,
                        after: cursor,
                        records: page.to_vec(),
                    },
                };
                assert_eq!(dispatch(&wire, &gkey, &fkey, &[0; 32]), Response::Loaded);
                for record in page {
                    cursor = advance(cursor, record).unwrap();
                }
            }
            let recovery_us = recovery.elapsed().as_micros();
            let request = EnclaveRequest::Confidential {
                request: Box::new(Request {
                    chain_id: chain,
                    gratis: head,
                    fidelity: Head::default(),
                    call: Call::Collateral {
                        authorization: CollateralAuthorization {
                            credis_id: U256::ONE,
                            collateral_id: B256::from(U256::ONE),
                            action: CollateralAction::Return,
                            amount: U256::ONE,
                            expected_remaining: U256::from(100),
                        },
                        timestamp: 1,
                        fidelity_anchor: 0,
                    },
                }),
            };
            let mut times = Vec::new();
            for _ in 0..101 {
                let started = Instant::now();
                let Response::Applied(result) = dispatch(&request, &gkey, &fkey, &[0; 32]) else {
                    panic!("benchmark operation rejected");
                };
                times.push(started.elapsed().as_micros());
                assert_eq!(result.updates.len(), 1);
                assert_eq!(result.updates[0].record.len(), RECORD_BYTES);
            }
            times.sort_unstable();
            eprintln!("journal population={population} median_operation_us={} recovery_us={recovery_us} journal_bytes={}", times[50], records.len() * RECORD_BYTES);
        }
    }
}
