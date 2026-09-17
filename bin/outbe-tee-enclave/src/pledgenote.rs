//! Enclave-resident confidential ledger, reconstructible from the chain journal.
//! No account address or account-keyed ciphertext is returned by private actions.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_tee::pledgenote::*;
use outbe_tee::protocol::{FidelityCohortOp, GratisOp};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::crypto::{chacha20poly1305_decrypt, chacha20poly1305_encrypt, hkdf_sha256};
use crate::fidelity::CohortState;

type Result<T> = std::result::Result<T, String>;

#[cfg(test)]
#[path = "pledgenote_tests.rs"]
mod tests;

/// One reconstructible cache shared by authenticated connections. It contains
/// no authority to commit: every operation must name its chain journal parent.
static CACHE: std::sync::OnceLock<std::sync::Mutex<Ledger>> = std::sync::OnceLock::new();

pub(crate) fn dispatch(
    key: &[u8; 32],
    offer_secret: &[u8; 32],
    request: &Request,
) -> Result<Response> {
    let inputs_hash = request_hash(request)?;
    let mut ledger = CACHE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "ledger cache poisoned")?;
    let reply = ledger.apply(key, offer_secret, request)?;
    Ok(Response {
        inputs_hash,
        reply,
        attestation: Vec::new(),
    })
}

pub(crate) fn replay(
    key: &[u8; 32],
    offer_secret: &[u8; 32],
    request: &ReplayRequest,
) -> Result<Response> {
    let inputs_hash = replay_hash(request)?;
    let mut ledger = CACHE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "ledger cache poisoned")?;
    if request.reset {
        if request.parent != Head::default() {
            return Err("replay reset requires genesis parent".into());
        }
        *ledger = Ledger::default();
    }
    let reply = if ledger.head != request.parent {
        Reply::NeedsReplay
    } else {
        ledger.replay(key, offer_secret, &request.entries)?;
        Reply::Applied(Box::new(Outcome {
            head: ledger.head,
            ..Default::default()
        }))
    };
    Ok(Response {
        inputs_hash,
        reply,
        attestation: Vec::new(),
    })
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Account {
    balance: U256,
    pledged: U256,
    nonce: u64,
    cohorts: CohortState,
}

#[derive(Clone, Serialize, Deserialize)]
struct Note {
    owner: Address,
    terms: Terms,
    secret: B256,
    reservation_id: B256,
}

#[derive(Clone, Serialize, Deserialize)]
struct Allocation {
    owner: Address,
    remaining: U256,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    head: Head,
    identity: Option<(B256, B256)>,
    accounts: BTreeMap<Address, Account>,
    notes: BTreeMap<B256, Note>,
    allocations: BTreeMap<B256, Allocation>,
    supply: U256,
    pledged_supply: U256,
    first_qualified_start: u64,
    // Canonical entry bytes; 256 bytes separately cover scalar fields/map lengths.
    entry_bytes: usize,
}

fn add(a: U256, b: U256) -> Result<U256> {
    a.checked_add(b)
        .ok_or_else(|| "confidential amount overflow".into())
}

fn sub(a: U256, b: U256) -> Result<U256> {
    a.checked_sub(b)
        .ok_or_else(|| "insufficient confidential balance".into())
}

fn derive(key: &[u8; 32], input: B256, domain: &[u8]) -> Result<B256> {
    hkdf_sha256(key, input.as_slice(), domain)
        .map(B256::from)
        .map_err(|e| e.to_string())
}

fn authentic(key: &[u8], expected: &[u8], actual: &[u8]) -> bool {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::verify(&key, actual, ring::hmac::sign(&key, expected).as_ref()).is_ok()
}

fn private_request(secret: &[u8; 32], bytes: &[u8]) -> Result<PrivateRequest> {
    if bytes.len() != 32 + 12 + ENVELOPE_PLAINTEXT_BYTES + 16 {
        return Err("invalid private request length".into());
    }
    let public: [u8; 32] = bytes[..32]
        .try_into()
        .map_err(|_| "invalid ephemeral key")?;
    let nonce: [u8; 12] = bytes[32..44]
        .try_into()
        .map_err(|_| "invalid envelope nonce")?;
    let shared = x25519_dalek::StaticSecret::from(*secret)
        .diffie_hellman(&x25519_dalek::PublicKey::from(public));
    if !shared.was_contributory() {
        return Err("invalid ephemeral key".into());
    }
    let key = Zeroizing::new(
        hkdf_sha256(ENVELOPE_DOMAIN, shared.as_bytes(), ENVELOPE_DOMAIN)
            .map_err(|e| e.to_string())?,
    );
    let plain = Zeroizing::new(
        chacha20poly1305_decrypt(&key, &nonce, &bytes[44..])
            .map_err(|_| "invalid private request authentication")?,
    );
    decode(unpad(&plain, ENVELOPE_PLAINTEXT_BYTES)?)
}

struct Undo {
    account: Option<(Address, Option<Account>)>,
    note: Option<(B256, Option<Note>)>,
    allocation: Option<(B256, Option<Allocation>)>,
    supply: U256,
    pledged_supply: U256,
    first_qualified_start: u64,
}

fn entry_size<K: Serialize, V: Serialize>(entry: Option<(&K, &V)>) -> Result<usize> {
    entry.map_or(Ok(0), |entry| encode(&entry).map(|v| v.len()))
}

impl Undo {
    fn entry_size(&self) -> Result<usize> {
        Ok(entry_size(
            self.account
                .as_ref()
                .and_then(|(k, v)| v.as_ref().map(|v| (k, v))),
        )? + entry_size(
            self.note
                .as_ref()
                .and_then(|(k, v)| v.as_ref().map(|v| (k, v))),
        )? + entry_size(
            self.allocation
                .as_ref()
                .and_then(|(k, v)| v.as_ref().map(|v| (k, v))),
        )?)
    }
    fn restore(self, ledger: &mut Ledger) {
        if let Some((k, v)) = self.account {
            if let Some(v) = v {
                ledger.accounts.insert(k, v);
            } else {
                ledger.accounts.remove(&k);
            }
        }
        if let Some((k, v)) = self.note {
            if let Some(v) = v {
                ledger.notes.insert(k, v);
            } else {
                ledger.notes.remove(&k);
            }
        }
        if let Some((k, v)) = self.allocation {
            if let Some(v) = v {
                ledger.allocations.insert(k, v);
            } else {
                ledger.allocations.remove(&k);
            }
        }
        ledger.supply = self.supply;
        ledger.pledged_supply = self.pledged_supply;
        ledger.first_qualified_start = self.first_qualified_start;
    }
}

impl Ledger {
    fn undo(
        &self,
        key: &[u8; 32],
        secret: &[u8; 32],
        request: &Request,
        input: B256,
    ) -> Result<Undo> {
        let (owner, note, allocation) = match &request.command {
            Command::Create { envelope, .. } => (
                Some(self.authorize(key, secret, request, envelope)?.0),
                Some(derive(key, input, b"outbe/pledgenote/note-id/v1")?),
                None,
            ),
            Command::Cancel { envelope } => {
                let (owner, action) = self.authorize(key, secret, request, envelope)?;
                let OwnerAction::Cancel { note_id } = action else {
                    return Err("cancel authorization required".into());
                };
                (Some(owner), Some(note_id), None)
            }
            Command::Use { envelope, .. } => {
                let PrivateRequest::Use { note_id, .. } = private_request(secret, envelope)? else {
                    return Err("use authorization required".into());
                };
                (
                    None,
                    Some(note_id),
                    Some(derive(
                        key,
                        input,
                        b"outbe/pledgenote/collateral-handle/v1",
                    )?),
                )
            }
            Command::Release {
                collateral_handle, ..
            }
            | Command::Forfeit {
                collateral_handle, ..
            } => {
                let owner = self
                    .allocations
                    .get(collateral_handle)
                    .ok_or("collateral allocation unavailable")?
                    .owner;
                (Some(owner), None, Some(*collateral_handle))
            }
            Command::Gratis { account, .. } | Command::Cohort { account, .. } => {
                (Some(*account), None, None)
            }
            _ => return Err("read-only command cannot mutate ledger".into()),
        };
        Ok(Undo {
            account: owner.map(|k| (k, self.accounts.get(&k).cloned())),
            note: note.map(|k| (k, self.notes.get(&k).cloned())),
            allocation: allocation.map(|k| (k, self.allocations.get(&k).cloned())),
            supply: self.supply,
            pledged_supply: self.pledged_supply,
            first_qualified_start: self.first_qualified_start,
        })
    }

    fn entries_size(&self, undo: &Undo) -> Result<usize> {
        Ok(entry_size(
            undo.account
                .as_ref()
                .and_then(|(k, _)| self.accounts.get(k).map(|v| (k, v))),
        )? + entry_size(
            undo.note
                .as_ref()
                .and_then(|(k, _)| self.notes.get(k).map(|v| (k, v))),
        )? + entry_size(
            undo.allocation
                .as_ref()
                .and_then(|(k, _)| self.allocations.get(k).map(|v| (k, v))),
        )?)
    }

    pub fn head(&self) -> Head {
        self.head
    }

    /// The caller supplies the authoritative parent from its execution frame.
    /// A speculative cache head is never authority: mismatch demands replay.
    pub fn apply(
        &mut self,
        key: &[u8; 32],
        offer_secret: &[u8; 32],
        request: &Request,
    ) -> Result<Reply> {
        if request.schema != SCHEMA_VERSION {
            return Ok(Reply::Rejected("unsupported ledger schema".into()));
        }
        if request.parent != self.head {
            return Ok(Reply::NeedsReplay);
        }
        let identity = (request.context.chain_id, request.context.genesis_hash);
        if self.identity.is_some_and(|id| id != identity) {
            return Err("ledger network mismatch".into());
        }
        let input = request_hash(request)?;
        if request.command.is_read_only() {
            let mut outcome = match self.execute(key, offer_secret, request, input) {
                Ok(outcome) => outcome,
                Err(error) => return Ok(Reply::Rejected(error)),
            };
            outcome.head = self.head;
            return Ok(Reply::Applied(Box::new(outcome)));
        }
        // Copy only entries touched by this command, never the whole ledger.
        // Rejection restores them before another connection can observe the cache.
        let undo = match self.undo(key, offer_secret, request, input) {
            Ok(undo) => undo,
            Err(error) => return Ok(Reply::Rejected(error)),
        };
        let previous_size = undo.entry_size()?;
        let result = (|| -> Result<Outcome> {
            let mut outcome = self.execute(key, offer_secret, request, input)?;
            let current_size = self.entries_size(&undo)?;
            let size = self
                .entry_bytes
                .checked_sub(previous_size)
                .and_then(|v| v.checked_add(current_size))
                .ok_or("ledger size overflow")?;
            // Half the budget is reserved for completion. Closing a note removes
            // it; conversion of every active cohort grows records by <2x,
            // and each extra partial-sale record is smaller than its removed allocation.
            let completion = matches!(
                request.command,
                Command::Release { .. }
                    | Command::Forfeit { .. }
                    | Command::Cancel { .. }
                    | Command::Use { .. }
            );
            let budget = if completion {
                LIVE_STATE_BUDGET
            } else {
                LIVE_STATE_BUDGET / 2
            };
            if size.checked_add(256).ok_or("ledger size overflow")? > budget {
                return Err("confidential live-state capacity reached".into());
            }
            let plain = Zeroizing::new(pad(&encode(request)?, JOURNAL_PLAINTEXT_BYTES)?);
            // Per-input keys prevent nonce reuse across forks and simulations.
            let journal_key = Zeroizing::new(derive(key, input, b"outbe/pledgenote/journal/v1")?.0);
            let encrypted = chacha20poly1305_encrypt(&journal_key, &[0u8; 12], &plain)
                .map_err(|e| e.to_string())?;
            let mut entry = input.as_slice().to_vec();
            entry.extend(encrypted);
            let next_head = Head {
                sequence: self
                    .head
                    .sequence
                    .checked_add(1)
                    .ok_or("ledger sequence exhausted")?,
                root: keccak256(&entry),
            };
            self.entry_bytes = size;
            self.identity = Some(identity);
            self.head = next_head;
            outcome.journal_entry = entry;
            Ok(outcome)
        })();
        let mut outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                undo.restore(self);
                return Ok(Reply::Rejected(error));
            }
        };
        outcome.head = self.head;
        outcome.total_supply = self.supply;
        outcome.pledged_supply = self.pledged_supply;
        outcome.first_qualified_start = self.first_qualified_start;
        Ok(Reply::Applied(Box::new(outcome)))
    }

    /// Replay committed entries in sequence. Truncation/reordering/splicing is
    /// rejected through each entry's authenticated parent, identity and hash.
    pub fn replay(
        &mut self,
        key: &[u8; 32],
        offer_secret: &[u8; 32],
        entries: &[Vec<u8>],
    ) -> Result<()> {
        if entries.len() > REPLAY_BATCH_ENTRIES {
            return Err("ledger replay batch too large".into());
        }
        for entry in entries {
            if entry.len() != 32 + JOURNAL_PLAINTEXT_BYTES + 16 {
                return Err("invalid journal entry length".into());
            }
            let input = B256::from_slice(&entry[..32]);
            let journal_key = Zeroizing::new(derive(key, input, b"outbe/pledgenote/journal/v1")?.0);
            let plain = Zeroizing::new(
                chacha20poly1305_decrypt(&journal_key, &[0u8; 12], &entry[32..])
                    .map_err(|_| "journal authentication failed")?,
            );
            let request: Request = decode(unpad(&plain, JOURNAL_PLAINTEXT_BYTES)?)?;
            if request_hash(&request)? != input || request.command.is_read_only() {
                return Err("invalid journal command".into());
            }
            match self.apply(key, offer_secret, &request)? {
                Reply::Applied(result) if result.journal_entry == *entry => {}
                _ => return Err("journal replay diverged".into()),
            }
        }
        Ok(())
    }

    fn authorize(
        &self,
        key: &[u8; 32],
        secret: &[u8; 32],
        request: &Request,
        envelope: &[u8],
    ) -> Result<(Address, OwnerAction)> {
        let PrivateRequest::Owner {
            chain_id,
            account,
            nonce,
            action,
            mac,
        } = private_request(secret, envelope)?
        else {
            return Err("owner authorization required".into());
        };
        if chain_id != request.context.chain_id || account.is_zero() {
            return Err("invalid owner context".into());
        }
        let modify = Zeroizing::new(
            crate::gratis::derive_modify_key(key, account).map_err(|e| e.to_string())?,
        );
        let expected = owner_mac(&modify, chain_id, account, nonce, &action)?;
        // ring verification is constant time, including the MAC comparison.
        let compare_key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &modify[..]);
        let comparison = ring::hmac::sign(&compare_key, expected.as_slice());
        ring::hmac::verify(&compare_key, mac.as_slice(), comparison.as_ref())
            .map_err(|_| "invalid owner authorization")?;
        let current = self.accounts.get(&account).map_or(0, |a| a.nonce);
        if !matches!(action, OwnerAction::Query | OwnerAction::QueryAt { .. }) && current != nonce {
            return Err("invalid owner nonce".into());
        }
        Ok((account, action))
    }

    fn receipt(
        &self,
        key: &[u8; 32],
        owner: Address,
        input: B256,
        note: Option<(B256, &Note)>,
        now: u64,
    ) -> Result<Vec<u8>> {
        let empty = Account::default();
        let account = self.accounts.get(&owner).unwrap_or(&empty);
        let (rcfi, efficiency, league) = account
            .cohorts
            .evaluate(now, self.first_qualified_start)
            .map_err(|e| e.to_string())?;
        let (note_id, secret, terms) = note.map_or((B256::ZERO, B256::ZERO, None), |(id, n)| {
            (id, n.secret, Some(n.terms.clone()))
        });
        let receipt = Receipt {
            note_id,
            secret,
            terms,
            balance: account.balance,
            pledged: account.pledged,
            next_nonce: account.nonce,
            rcfi,
            efficiency,
            league,
        };
        let view_key =
            Zeroizing::new(crate::gratis::derive_view_key(key, owner).map_err(|e| e.to_string())?);
        let nonce_material = derive(key, input, b"outbe/pledgenote/receipt-nonce/v1")?;
        let nonce: [u8; 12] = nonce_material[..12]
            .try_into()
            .map_err(|_| "receipt nonce")?;
        let plaintext = Zeroizing::new(pad(&encode(&receipt)?, ENVELOPE_PLAINTEXT_BYTES)?);
        let mut ciphertext = nonce.to_vec();
        ciphertext.extend(
            chacha20poly1305_encrypt(&view_key, &nonce, &plaintext).map_err(|e| e.to_string())?,
        );
        Ok(ciphertext)
    }

    fn cohort(&mut self, owner: Address, amount: U256, op: FidelityCohortOp, now: u64) {
        if amount.is_zero() {
            return;
        }
        let account = self.accounts.entry(owner).or_default();
        match op {
            FidelityCohortOp::In => {
                account.cohorts.cohort_in(amount, now);
                if self.first_qualified_start == 0 {
                    self.first_qualified_start = now;
                }
            }
            FidelityCohortOp::Out => account.cohorts.cohort_out(amount, now),
            FidelityCohortOp::Probe => {}
        }
    }

    fn execute(
        &mut self,
        key: &[u8; 32],
        secret: &[u8; 32],
        request: &Request,
        input: B256,
    ) -> Result<Outcome> {
        let now = request.context.timestamp;
        let mut out = Outcome::default();
        match &request.command {
            Command::Create {
                quote,
                terms,
                envelope,
            } => {
                let (owner, action) = self.authorize(key, secret, request, envelope)?;
                if action != OwnerAction::Create(quote.clone())
                    || terms.asset != quote.asset
                    || terms.principal_minor != quote.principal_minor
                    || terms.reference_currency != quote.reference_currency
                    || terms.gratis_minor > quote.max_gratis_minor
                    || terms.gratis_minor.is_zero()
                    || terms.principal_minor.is_zero()
                    || terms.asset.is_zero()
                    || terms.entry_price_minor.is_zero()
                    || terms.created_at != now
                    || now.checked_add(QUOTE_TTL_SECONDS) != Some(terms.valid_until)
                {
                    return Err("invalid pledge quote".into());
                }
                let account = self.accounts.entry(owner).or_default();
                account.balance = sub(account.balance, terms.gratis_minor)?;
                account.pledged = add(account.pledged, terms.gratis_minor)?;
                account.nonce = account
                    .nonce
                    .checked_add(1)
                    .ok_or("owner nonce exhausted")?;
                self.pledged_supply = add(self.pledged_supply, terms.gratis_minor)?;
                let id = derive(key, input, b"outbe/pledgenote/note-id/v1")?;
                let note = Note {
                    owner,
                    terms: terms.clone(),
                    secret: derive(key, input, b"outbe/pledgenote/spend-secret/v1")?,
                    reservation_id: derive(key, input, b"outbe/pledgenote/reservation-id/v1")?,
                };
                out.reservation_id = note.reservation_id;
                out.encrypted_receipt = self.receipt(key, owner, input, Some((id, &note)), now)?;
                out.terms = Some(terms.clone());
                self.notes.insert(id, note);
            }
            Command::Cancel { envelope } => {
                let (owner, action) = self.authorize(key, secret, request, envelope)?;
                let OwnerAction::Cancel { note_id } = action else {
                    return Err("cancel authorization required".into());
                };
                let note = self.notes.get(&note_id).ok_or("note unavailable")?.clone();
                if note.owner != owner {
                    return Err("note unavailable".into());
                }
                let account = self.accounts.get_mut(&owner).ok_or("missing note owner")?;
                account.balance = add(account.balance, note.terms.gratis_minor)?;
                account.pledged = sub(account.pledged, note.terms.gratis_minor)?;
                account.nonce = account
                    .nonce
                    .checked_add(1)
                    .ok_or("owner nonce exhausted")?;
                self.pledged_supply = sub(self.pledged_supply, note.terms.gratis_minor)?;
                self.notes.remove(&note_id);
                out.reservation_id = note.reservation_id;
                out.amount = note.terms.gratis_minor;
                out.encrypted_receipt = self.receipt(key, owner, input, None, now)?;
            }
            Command::Use { owner_sa, envelope } => {
                let PrivateRequest::Use {
                    chain_id,
                    note_id,
                    owner_sa: bound_sa,
                    authorization,
                } = private_request(secret, envelope)?
                else {
                    return Err("use authorization required".into());
                };
                let note = self.notes.get(&note_id).ok_or("note unavailable")?.clone();
                if chain_id != request.context.chain_id
                    || *owner_sa != bound_sa
                    || owner_sa.is_zero()
                    || !authentic(
                        note.secret.as_slice(),
                        use_mac(note.secret, chain_id, note_id, *owner_sa)?.as_slice(),
                        authorization.as_slice(),
                    )
                {
                    return Err("invalid use authorization".into());
                }
                if now > note.terms.valid_until {
                    return Err("pledge note expired".into());
                }
                self.notes.remove(&note_id);
                out.collateral_handle =
                    derive(key, input, b"outbe/pledgenote/collateral-handle/v1")?;
                out.credis_id = derive(key, input, b"outbe/pledgenote/credis-id/v1")?;
                out.reservation_id = note.reservation_id;
                out.terms = Some(note.terms.clone());
                self.allocations.insert(
                    out.collateral_handle,
                    Allocation {
                        owner: note.owner,
                        remaining: note.terms.gratis_minor,
                    },
                );
            }
            Command::Release {
                collateral_handle,
                amount,
            }
            | Command::Forfeit {
                collateral_handle,
                amount,
            } => {
                if amount.is_zero() {
                    return Err("zero collateral transition".into());
                }
                let mut allocation = self
                    .allocations
                    .get(collateral_handle)
                    .ok_or("collateral allocation unavailable")?
                    .clone();
                if matches!(request.command, Command::Forfeit { .. })
                    && *amount != allocation.remaining
                {
                    return Err("forfeiture must close the remaining allocation".into());
                }
                allocation.remaining = sub(allocation.remaining, *amount)?;
                let account = self
                    .accounts
                    .get_mut(&allocation.owner)
                    .ok_or("missing collateral owner")?;
                account.pledged = sub(account.pledged, *amount)?;
                self.pledged_supply = sub(self.pledged_supply, *amount)?;
                if matches!(request.command, Command::Release { .. }) {
                    account.balance = add(account.balance, *amount)?;
                } else {
                    self.supply = sub(self.supply, *amount)?;
                    self.cohort(allocation.owner, *amount, FidelityCohortOp::Out, now);
                }
                if allocation.remaining.is_zero() {
                    self.allocations.remove(collateral_handle);
                } else {
                    self.allocations.insert(*collateral_handle, allocation);
                }
                out.amount = *amount;
            }
            Command::Gratis {
                account: owner,
                amount,
                op,
                auth,
                fidelity,
            } => {
                if !matches!(op, GratisOp::Mint | GratisOp::Burn) || amount.is_zero() {
                    return Err("invalid gratis command".into());
                }
                let modify = Zeroizing::new(
                    crate::gratis::derive_modify_key(key, *owner).map_err(|e| e.to_string())?,
                );
                if !authentic(
                    &modify[..],
                    &crate::gratis::modify_mac(
                        &modify,
                        *owner,
                        *op,
                        *amount,
                        auth.op_nonce,
                        request.context.chain_id,
                    ),
                    &auth.mac,
                ) {
                    return Err("invalid gratis authorization".into());
                }
                let account = self.accounts.entry(*owner).or_default();
                if account.nonce != auth.op_nonce {
                    return Err("invalid owner nonce".into());
                }
                account.nonce = account
                    .nonce
                    .checked_add(1)
                    .ok_or("owner nonce exhausted")?;
                if *op == GratisOp::Mint {
                    account.balance = add(account.balance, *amount)?;
                    self.supply = add(self.supply, *amount)?;
                } else {
                    account.balance = sub(account.balance, *amount)?;
                    self.supply = sub(self.supply, *amount)?;
                }
                if *fidelity {
                    self.cohort(
                        *owner,
                        *amount,
                        if *op == GratisOp::Mint {
                            FidelityCohortOp::In
                        } else {
                            FidelityCohortOp::Out
                        },
                        now,
                    );
                }
                out.amount = *amount;
            }
            Command::Cohort {
                account,
                amount,
                op,
                timestamp,
            } => {
                if *timestamp > now {
                    return Err("future cohort timestamp".into());
                }
                self.cohort(*account, *amount, *op, *timestamp);
            }
            Command::Query { envelope } => {
                let (owner, action) = self.authorize(key, secret, request, envelope)?;
                let at = match action {
                    OwnerAction::Query => now,
                    OwnerAction::QueryAt { timestamp } => timestamp,
                    _ => return Err("query authorization required".into()),
                };
                out.encrypted_receipt = self.receipt(key, owner, input, None, at)?;
            }
            Command::Snapshot { owners, timestamp } => {
                if owners.len() > SNAPSHOT_BATCH_OWNERS {
                    return Err("snapshot batch too large".into());
                }
                for owner in owners {
                    let empty = Account::default();
                    let account = self.accounts.get(owner).unwrap_or(&empty);
                    let (_, _, league) = account
                        .cohorts
                        .evaluate(*timestamp, self.first_qualified_start)
                        .map_err(|e| e.to_string())?;
                    out.leagues.push((*owner, league));
                }
            }
        }
        Ok(out)
    }
}
