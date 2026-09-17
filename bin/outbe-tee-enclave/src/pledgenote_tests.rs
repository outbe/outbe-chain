use super::*;
use outbe_tee::protocol::ModifyAuth;

const KEY: [u8; 32] = [7; 32];
const OFFER: [u8; 32] = [9; 32];
const OWNER: Address = Address::new([0xA1; 20]);
const SA: Address = Address::new([0xB2; 20]);
const CHAIN: B256 = B256::repeat_byte(1);

fn request(ledger: &Ledger, command: Command, now: u64) -> Request {
    Request {
        schema: SCHEMA_VERSION,
        parent: ledger.head(),
        context: Context {
            chain_id: CHAIN,
            genesis_hash: B256::repeat_byte(3),
            block_number: 1,
            timestamp: now,
        },
        command,
    }
}

fn apply(ledger: &mut Ledger, command: Command, now: u64, journal: &mut Vec<Vec<u8>>) -> Outcome {
    let req = request(ledger, command, now);
    match ledger.apply(&KEY, &OFFER, &req).unwrap() {
        Reply::Applied(outcome) => {
            if !outcome.journal_entry.is_empty() {
                journal.push(outcome.journal_entry.clone());
            }
            *outcome
        }
        other => panic!("unexpected ledger reply: {other:?}"),
    }
}

fn envelope(request: PrivateRequest) -> Vec<u8> {
    encrypt_request(crate::crypto::x25519_public(&OFFER), &request).unwrap()
}

fn owner_envelope(nonce: u64, action: OwnerAction) -> Vec<u8> {
    let modify = crate::gratis::derive_modify_key(&KEY, OWNER).unwrap();
    let mac = owner_mac(&modify, CHAIN, OWNER, nonce, &action).unwrap();
    envelope(PrivateRequest::Owner {
        chain_id: CHAIN,
        account: OWNER,
        nonce,
        action,
        mac,
    })
}

fn mint(ledger: &mut Ledger, journal: &mut Vec<Vec<u8>>) {
    let amount = U256::from(1000);
    let modify = crate::gratis::derive_modify_key(&KEY, OWNER).unwrap();
    let auth = ModifyAuth {
        mac: crate::gratis::modify_mac(&modify, OWNER, GratisOp::Mint, amount, 0, CHAIN),
        op_nonce: 0,
    };
    apply(
        ledger,
        Command::Gratis {
            account: OWNER,
            amount,
            op: GratisOp::Mint,
            auth,
            fidelity: true,
        },
        1000,
        journal,
    );
}

fn create(ledger: &mut Ledger, journal: &mut Vec<Vec<u8>>) -> (Outcome, Receipt) {
    let quote = Quote {
        asset: Address::repeat_byte(4),
        principal_minor: U256::from(200),
        max_gratis_minor: U256::from(100),
        reference_currency: 978,
    };
    let terms = Terms {
        asset: quote.asset,
        principal_minor: quote.principal_minor,
        gratis_minor: U256::from(100),
        issuance_currency: 840,
        reference_currency: 978,
        entry_price_minor: U256::from(2_000_000),
        created_at: 1000,
        valid_until: 1900,
    };
    let encrypted = owner_envelope(1, OwnerAction::Create(quote.clone()));
    let outcome = apply(
        ledger,
        Command::Create {
            quote,
            terms,
            envelope: encrypted,
        },
        1000,
        journal,
    );
    let view = crate::gratis::derive_view_key(&KEY, OWNER).unwrap();
    let receipt = decrypt_receipt(&view, &outcome.encrypted_receipt).unwrap();
    (outcome, receipt)
}

fn use_envelope(receipt: &Receipt) -> Vec<u8> {
    envelope(PrivateRequest::Use {
        chain_id: CHAIN,
        note_id: receipt.note_id,
        owner_sa: SA,
        authorization: use_mac(receipt.secret, CHAIN, receipt.note_id, SA).unwrap(),
    })
}

#[test]
fn lifecycle_replays_with_private_owner_and_fixed_journal_size() {
    let mut ledger = Ledger::default();
    let mut journal = Vec::new();
    mint(&mut ledger, &mut journal);
    let (created, receipt) = create(&mut ledger, &mut journal);
    assert_eq!(receipt.balance, U256::from(900));
    assert_eq!(receipt.pledged, U256::from(100));
    let used = apply(
        &mut ledger,
        Command::Use {
            owner_sa: SA,
            envelope: use_envelope(&receipt),
        },
        1900,
        &mut journal,
    );
    assert_eq!(used.reservation_id, created.reservation_id);
    assert_ne!(used.credis_id, receipt.note_id);
    assert_ne!(used.collateral_handle, receipt.note_id);
    assert!(ledger.notes.is_empty());
    apply(
        &mut ledger,
        Command::Release {
            collateral_handle: used.collateral_handle,
            amount: U256::from(33),
        },
        2000,
        &mut journal,
    );
    apply(
        &mut ledger,
        Command::Forfeit {
            collateral_handle: used.collateral_handle,
            amount: U256::from(67),
        },
        3000,
        &mut journal,
    );
    assert!(ledger.allocations.is_empty());
    assert_eq!(ledger.accounts[&OWNER].balance, U256::from(933));
    assert_eq!(ledger.accounts[&OWNER].pledged, U256::ZERO);
    assert_eq!(ledger.supply, U256::from(933));
    assert_eq!(ledger.pledged_supply, U256::ZERO);
    for entry in &journal {
        assert_eq!(entry.len(), 32 + JOURNAL_PLAINTEXT_BYTES + 16);
        assert!(!entry.windows(20).any(|bytes| bytes == OWNER.as_slice()));
    }
    let mut recovered = Ledger::default();
    recovered.replay(&KEY, &OFFER, &journal).unwrap();
    assert_eq!(encode(&recovered).unwrap(), encode(&ledger).unwrap());
    let before = ledger.head();
    let result = apply(
        &mut ledger,
        Command::Query {
            envelope: owner_envelope(0, OwnerAction::Query),
        },
        3000,
        &mut journal,
    );
    assert_eq!(before, ledger.head());
    let view = crate::gratis::derive_view_key(&KEY, OWNER).unwrap();
    assert_eq!(
        decrypt_receipt(&view, &result.encrypted_receipt)
            .unwrap()
            .balance,
        U256::from(933)
    );
}

#[test]
fn expired_note_can_cancel_and_replay_cannot_resurrect_it() {
    let mut ledger = Ledger::default();
    let mut journal = Vec::new();
    mint(&mut ledger, &mut journal);
    let (_, receipt) = create(&mut ledger, &mut journal);
    let head = ledger.head();
    let expired = request(
        &ledger,
        Command::Use {
            owner_sa: SA,
            envelope: use_envelope(&receipt),
        },
        1901,
    );
    assert!(matches!(
        ledger.apply(&KEY, &OFFER, &expired).unwrap(),
        Reply::Rejected(_)
    ));
    assert_eq!(ledger.head(), head);
    let command = Command::Cancel {
        envelope: owner_envelope(
            2,
            OwnerAction::Cancel {
                note_id: receipt.note_id,
            },
        ),
    };
    let cancelled = request(&ledger, command.clone(), 2000);
    apply(&mut ledger, command, 2000, &mut journal);
    assert_eq!(ledger.accounts[&OWNER].balance, U256::from(1000));
    assert_eq!(
        ledger.apply(&KEY, &OFFER, &cancelled).unwrap(),
        Reply::NeedsReplay
    );
    let mut recovered = Ledger::default();
    recovered.replay(&KEY, &OFFER, &journal).unwrap();
    assert!(recovered.notes.is_empty());
    let mut corrupt = journal.clone();
    corrupt[1][90] ^= 1;
    assert!(Ledger::default().replay(&KEY, &OFFER, &corrupt).is_err());
    journal.swap(0, 1);
    assert!(Ledger::default().replay(&KEY, &OFFER, &journal).is_err());
}

#[test]
fn forks_do_not_reuse_journal_keys() {
    let mut ledger = Ledger::default();
    let mut journal = Vec::new();
    mint(&mut ledger, &mut journal);
    let (_, receipt) = create(&mut ledger, &mut journal);
    let mut alternate = ledger.clone();
    let command = Command::Cancel {
        envelope: owner_envelope(
            2,
            OwnerAction::Cancel {
                note_id: receipt.note_id,
            },
        ),
    };
    let a = apply(&mut ledger, command.clone(), 2000, &mut journal);
    let b = apply(&mut alternate, command, 2001, &mut Vec::new());
    assert_ne!(&a.journal_entry[..32], &b.journal_entry[..32]);
    assert_ne!(a.head.root, b.head.root);
    assert_eq!(a.head.sequence, b.head.sequence);
}

#[test]
fn capacity_rejection_rolls_back_and_completion_uses_reserved_headroom() {
    let mut ledger = Ledger::default();
    let mut journal = Vec::new();
    mint(&mut ledger, &mut journal);
    let (_, receipt) = create(&mut ledger, &mut journal);
    // Model an otherwise-full ledger; this command may touch only this owner.
    ledger.entry_bytes = LIVE_STATE_BUDGET / 2;
    let before = encode(&ledger).unwrap();
    let key = crate::gratis::derive_modify_key(&KEY, OWNER).unwrap();
    let amount = U256::ONE;
    let auth = ModifyAuth {
        mac: crate::gratis::modify_mac(&key, OWNER, GratisOp::Mint, amount, 2, CHAIN),
        op_nonce: 2,
    };
    let req = request(
        &ledger,
        Command::Gratis {
            account: OWNER,
            amount,
            op: GratisOp::Mint,
            auth,
            fidelity: true,
        },
        1100,
    );
    assert!(matches!(
        ledger.apply(&KEY, &OFFER, &req).unwrap(),
        Reply::Rejected(_)
    ));
    assert_eq!(encode(&ledger).unwrap(), before);
    apply(
        &mut ledger,
        Command::Cancel {
            envelope: owner_envelope(
                2,
                OwnerAction::Cancel {
                    note_id: receipt.note_id,
                },
            ),
        },
        2000,
        &mut journal,
    );
    assert_eq!(ledger.accounts[&OWNER].balance, U256::from(1000));
    assert_eq!(ledger.accounts[&OWNER].nonce, 3);
}

/// Portable measurement; run explicitly with --ignored --nocapture. SGX EPC/RSS
/// measurements must use the deployed enclave and are not inferred from this test.
#[test]
#[ignore = "capacity/replay measurement"]
fn measure_capacity_and_replay() {
    use std::time::Instant;
    let mut ledger = Ledger::default();
    let mut journal = Vec::new();
    mint(&mut ledger, &mut journal);
    let (_, receipt) = create(&mut ledger, &mut journal);
    let template = ledger.notes[&receipt.note_id].clone();
    let size = entry_size(Some((&receipt.note_id, &template))).unwrap();
    let mut next = 1u64;
    while ledger.entry_bytes + size + 256 < LIVE_STATE_BUDGET / 2 {
        let id = B256::from(U256::from(next));
        ledger.notes.insert(id, template.clone());
        ledger.entry_bytes += size;
        next += 1;
    }
    let started = Instant::now();
    apply(
        &mut ledger,
        Command::Cancel {
            envelope: owner_envelope(
                2,
                OwnerAction::Cancel {
                    note_id: receipt.note_id,
                },
            ),
        },
        2000,
        &mut journal,
    );
    println!(
        "capacity entries={} canonical_bytes={} cancel_us={}",
        ledger.notes.len(),
        ledger.entry_bytes + 256,
        started.elapsed().as_micros()
    );
    drop(ledger);

    let mut source = Ledger::default();
    journal.clear();
    mint(&mut source, &mut journal);
    let key = crate::gratis::derive_modify_key(&KEY, OWNER).unwrap();
    for nonce in 1..2000 {
        let amount = U256::ONE;
        let auth = ModifyAuth {
            mac: crate::gratis::modify_mac(&key, OWNER, GratisOp::Mint, amount, nonce, CHAIN),
            op_nonce: nonce,
        };
        apply(
            &mut source,
            Command::Gratis {
                account: OWNER,
                amount,
                op: GratisOp::Mint,
                auth,
                fidelity: false,
            },
            1000,
            &mut journal,
        );
    }
    let mut recovered = Ledger::default();
    let started = Instant::now();
    for batch in journal.chunks(REPLAY_BATCH_ENTRIES) {
        recovered.replay(&KEY, &OFFER, batch).unwrap();
    }
    assert_eq!(encode(&recovered).unwrap(), encode(&source).unwrap());
    println!(
        "replay entries={} journal_bytes={} elapsed_ms={}",
        journal.len(),
        journal.iter().map(Vec::len).sum::<usize>(),
        started.elapsed().as_millis()
    );
}

#[test]
fn use_cannot_redirect_or_replay_and_failed_release_preserves_allocation() {
    let mut ledger = Ledger::default();
    let mut journal = Vec::new();
    mint(&mut ledger, &mut journal);
    let (_, receipt) = create(&mut ledger, &mut journal);
    let envelope = use_envelope(&receipt);
    let before = encode(&ledger).unwrap();
    let redirected = request(
        &ledger,
        Command::Use {
            owner_sa: OWNER,
            envelope: envelope.clone(),
        },
        1100,
    );
    assert!(matches!(
        ledger.apply(&KEY, &OFFER, &redirected).unwrap(),
        Reply::Rejected(_)
    ));
    assert_eq!(encode(&ledger).unwrap(), before);
    let used = apply(
        &mut ledger,
        Command::Use {
            owner_sa: SA,
            envelope: envelope.clone(),
        },
        1100,
        &mut journal,
    );
    let committed = encode(&ledger).unwrap();
    let replay = request(
        &ledger,
        Command::Use {
            owner_sa: SA,
            envelope,
        },
        1101,
    );
    assert!(matches!(
        ledger.apply(&KEY, &OFFER, &replay).unwrap(),
        Reply::Rejected(_)
    ));
    let over_release = request(
        &ledger,
        Command::Release {
            collateral_handle: used.collateral_handle,
            amount: U256::from(101),
        },
        1102,
    );
    assert!(matches!(
        ledger.apply(&KEY, &OFFER, &over_release).unwrap(),
        Reply::Rejected(_)
    ));
    assert_eq!(encode(&ledger).unwrap(), committed);
}
