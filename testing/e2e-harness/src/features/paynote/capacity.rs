//! Old notes remain spendable after their deposit roots leave the acceptance window.

use std::collections::BTreeSet;
use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use cucumber::then;
use outbe_protocol::Codec as _;
use serde_json::Value;

use super::{new_tree, note_nullifier, prove_full_spend, read_tree, Note, PayNoteSuit};
use crate::features::gem_lifecycle::IGemTestArming;
use crate::features::settlement::{assert_mined_success, assert_receipt_event, fund_and_approve};
use crate::internal::{addresses, eth};
use crate::world::forge::DEPLOYER_KEY;
use crate::world::settlement_currency::USD_ISO;
use crate::world::test_issuance::{self, ITestToken, SeriesSpec};
use crate::world::{venue_probes, World};

// Counts are bounded fixture sizes, not economic amounts.
const NOTES: u32 = 1_000;
const GEMS_PER_POSITION: u32 = 25;
const DEPOSIT_BATCH: usize = 32;

#[then("1000 PayNotes deposited before any spend fully settle 1000 GEMs on every validator")]
fn thousand_notes_settle_gems(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let owner = crate::world::origin_venue::deployer_address();
    let currency = world
        .state
        .settlement_currency
        .expect("registered currency");
    let chain_id = world.rpc.chain_id(port).expect("chain ID");
    assert_eq!(
        read(
            &url,
            addresses::PAYNOTE_ADDR,
            &eth::IPayNote::leafCountCall {}
        ),
        0
    );

    let gems = issue_gems(world);
    let notes: Vec<_> = gems
        .iter()
        .map(|&gem_id| {
            let amount = read(
                &url,
                addresses::GEM_FACTORY_ADDR,
                &eth::IGemFactory::quoteSettlementCall {
                    gemId: gem_id,
                    asset: currency.asset,
                },
            )
            .payableUnits;
            assert!(!amount.is_zero(), "zero quote for GEM {gem_id}");
            Note::new(chain_id, currency.asset, amount)
        })
        .collect();
    assert_eq!(notes.len(), NOTES as usize);
    let commitments: BTreeSet<_> = notes.iter().map(|n| word(&n.commitment)).collect();
    let nullifiers: Vec<_> = notes
        .iter()
        .map(|n| word(&note_nullifier(n.commitment, n.spend_key).expect("nullifier")))
        .collect();
    assert_eq!(commitments.len(), notes.len());
    assert_eq!(
        nullifiers.iter().collect::<BTreeSet<_>>().len(),
        notes.len()
    );
    let total = notes
        .iter()
        .try_fold(U256::ZERO, |sum, n| sum.checked_add(n.amount))
        .expect("aggregate deposit fits U256");
    fund_and_approve(
        world,
        currency.asset,
        DEPLOYER_KEY,
        owner,
        addresses::PAYNOTE_ADDR,
        total,
    );
    let before = balances(&url, currency.asset, owner, currency.vault, head(&url));

    let mut tree = new_tree(chain_id).expect("empty tree");
    let mut deposited = 0;
    let mut first_root = B256::ZERO;
    let mut stale_proof = Vec::new();
    let mut last_deposit = None;
    while deposited < notes.len() {
        // Observe the exact 31/32-later-append boundary before larger batches.
        let end = match deposited {
            0 => 1,
            1 => 32,
            32 => 33,
            _ => (deposited + DEPOSIT_BATCH).min(notes.len()),
        };
        let calls = notes[deposited..end]
            .iter()
            .map(|note| {
                prepared(
                    addresses::PAYNOTE_ADDR,
                    &eth::IPayNote::depositCall {
                        asset: note.asset,
                        amount: note.amount,
                        noteSn: note.serial_word(),
                    },
                )
            })
            .collect();
        let outcomes = eth::send_prepared_calls_outcomes(&url, DEPLOYER_KEY, calls)
            .unwrap_or_else(|e| panic!("deposit notes {deposited}..{end}: {e:#}"));
        assert_eq!(outcomes.len(), end - deposited);
        for (index, outcome) in (deposited..end).zip(outcomes) {
            let note = &notes[index];
            eprintln!(
                "paynote_capacity phase=deposit note={index} tx={}",
                outcome.transaction_hash
            );
            assert_mined_success(&outcome, &format!("deposit note {index}"));
            tree.append(note.commitment).expect("append deposited note");
            assert_receipt_event(
                &outcome.receipt,
                addresses::PAYNOTE_ADDR,
                &eth::IPayNote::NewNote {
                    commitment: word(&note.commitment),
                    leafIndex: u32::try_from(index).expect("index is below 1000"),
                    rootAfter: word(&tree.root()),
                    asset: note.asset,
                    noteAmount: note.amount,
                },
            );
            assert_eq!(
                event_count::<eth::IPayNote::NewNote>(&outcome.receipt, addresses::PAYNOTE_ADDR),
                1
            );
            assert!(!read(
                &url,
                addresses::PAYNOTE_ADDR,
                &eth::IPayNote::isSpentCall {
                    nullifier: nullifiers[index]
                }
            ));
            if (index + 1) % 100 == 0 {
                eprintln!(
                    "paynote_capacity phase=deposit completed={} tx={}",
                    index + 1,
                    outcome.transaction_hash
                );
            }
            last_deposit = Some(outcome);
        }
        deposited = end;
        if deposited == 1 {
            first_root = word(&tree.root());
            stale_proof = prove_full_spend(&notes[0], owner, &tree);
        }
        if matches!(deposited, 32 | 33) {
            assert_eq!(
                read(
                    &url,
                    addresses::PAYNOTE_ADDR,
                    &eth::IPayNote::isKnownRootCall { root: first_root }
                ),
                deposited == 32,
                "first root acceptance at {deposited} deposits"
            );
        }
    }
    let deposited_height = finalize(world, last_deposit.as_ref().expect("1000 deposits"));
    let tree = read_tree(world, port, chain_id);
    let root = word(&tree.root());
    assert_eq!(tree.leaves().len(), notes.len());
    assert_eq!(
        read_at(
            &url,
            addresses::PAYNOTE_ADDR,
            &eth::IPayNote::currentRootCall {},
            deposited_height
        ),
        root
    );
    let funded = balances(
        &url,
        currency.asset,
        owner,
        currency.vault,
        deposited_height,
    );
    assert_eq!(
        funded,
        [before[0] - total, before[1] + total, before[2], before[3]],
        "only deposits move ERC20 from payer to reserve"
    );
    reject_without_mutation(
        world,
        gems[0],
        &stale_proof,
        nullifiers[0],
        "PayNote root is not recent",
    );

    let mut last_settlement = None;
    for (index, (&gem_id, note)) in gems.iter().zip(&notes).enumerate() {
        eprintln!("paynote_capacity phase=prove note={index} gem_id={gem_id}");
        let proof = prove_full_spend(note, owner, &tree);
        let outcome = send(
            &url,
            addresses::GEM_FACTORY_ADDR,
            &eth::IGemFactory::settleGemWithPayNoteCall {
                gemId: gem_id,
                payNoteProof: proof.clone().into(),
            },
        );
        eprintln!(
            "paynote_capacity phase=settle note={index} gem_id={gem_id} tx={}",
            outcome.transaction_hash
        );
        assert_mined_success(&outcome, &format!("settle note {index}, GEM {gem_id}"));
        assert_receipt_event(
            &outcome.receipt,
            addresses::PAYNOTE_ADDR,
            &eth::IPayNote::NoteUsed {
                asset: note.asset,
                owner,
                nullifier: nullifiers[index],
                spendAmount: note.amount,
            },
        );
        assert_receipt_event(
            &outcome.receipt,
            addresses::GEM_FACTORY_ADDR,
            &eth::IGemFactory::GemSettled {
                gemId: gem_id,
                owner,
                amountPaid: note.amount,
                settlementCurrency: USD_ISO,
            },
        );
        assert_eq!(
            event_count::<eth::IPayNote::NoteUsed>(&outcome.receipt, addresses::PAYNOTE_ADDR),
            1
        );
        assert_eq!(
            event_count::<eth::IGemFactory::GemSettled>(
                &outcome.receipt,
                addresses::GEM_FACTORY_ADDR
            ),
            1
        );
        assert_eq!(
            event_count::<eth::IPayNote::NewNote>(&outcome.receipt, addresses::PAYNOTE_ADDR),
            0
        );
        assert!(
            outcome.receipt["logs"]
                .as_array()
                .expect("receipt logs")
                .iter()
                .all(|log| log["address"] != format!("{:#x}", currency.asset)),
            "settlement must not transfer or approve ERC20 again: note {index}, GEM {gem_id}"
        );
        assert!(read(
            &url,
            addresses::PAYNOTE_ADDR,
            &eth::IPayNote::isSpentCall {
                nullifier: nullifiers[index]
            }
        ));
        let gem = read(
            &url,
            addresses::GEM_ADDR,
            &eth::IGem::getGemStatusCall { gemId: gem_id },
        );
        assert_eq!(
            (gem.state, gem.owner),
            (3, owner),
            "GEM {gem_id} must settle for its owner"
        );
        if index == 0 {
            reject_without_mutation(
                world,
                gems[1],
                &proof,
                nullifiers[0],
                "PayNote nullifier has already been spent",
            );
        }
        if (index + 1) % 100 == 0 {
            eprintln!(
                "paynote_capacity phase=settle completed={} gem_id={gem_id} tx={}",
                index + 1,
                outcome.transaction_hash
            );
        }
        last_settlement = Some(outcome);
    }

    let height = finalize(world, last_settlement.as_ref().expect("1000 settlements"));
    for port in world.validators.committee_ports() {
        let url = world.rpc.url(port);
        assert_eq!(
            read_at(
                &url,
                addresses::PAYNOTE_ADDR,
                &eth::IPayNote::leafCountCall {},
                height
            ),
            u64::from(NOTES)
        );
        assert_eq!(
            read_at(
                &url,
                addresses::PAYNOTE_ADDR,
                &eth::IPayNote::currentRootCall {},
                height
            ),
            root
        );
        assert_eq!(
            balances(&url, currency.asset, owner, currency.vault, height),
            funded
        );
        for (index, &gem_id) in gems.iter().enumerate() {
            assert!(
                read_at(
                    &url,
                    addresses::PAYNOTE_ADDR,
                    &eth::IPayNote::hasCommitmentCall {
                        commitment: word(&notes[index].commitment),
                    },
                    height
                ),
                "missing note {index} on validator RPC {port}"
            );
            assert!(
                read_at(
                    &url,
                    addresses::PAYNOTE_ADDR,
                    &eth::IPayNote::isSpentCall {
                        nullifier: nullifiers[index],
                    },
                    height
                ),
                "unspent note {index} on validator RPC {port}"
            );
            let gem = read_at(
                &url,
                addresses::GEM_ADDR,
                &eth::IGem::getGemStatusCall { gemId: gem_id },
                height,
            );
            assert_eq!(
                (gem.state, gem.owner),
                (3, owner),
                "GEM {gem_id}, validator RPC {port}"
            );
        }
    }
    eprintln!("paynote_capacity PASS deposited={NOTES} spent={NOTES} settled={NOTES} change_notes=0 amount={total} root={root:#x} finalized_height={height}");
}

fn issue_gems(world: &mut World) -> Vec<U256> {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let owner = crate::world::origin_venue::deployer_address();
    let contracts = world.state.origin_contracts.as_ref().expect("Intex engine");
    let (router, nft) = (contracts.origin_router, contracts.intex_nft);
    let now = world
        .rpc
        .latest_block_timestamp(port)
        .expect("block timestamp");
    let day = outbe_primitives::time::worldwide_day_from_timestamp(now);
    let issued_at = u32::try_from(now).expect("fixture timestamp fits Intex ABI uint32");
    // Existing Intex fixture ABI takes u128 load and u64 price; these literals fit.
    let entry = 1_000_000_u64;
    let load = 100_000_u128;
    test_issuance::open_day(
        &url,
        DEPLOYER_KEY,
        router,
        day,
        issued_at,
        USD_ISO,
        entry,
        load,
    )
    .expect("open source issuance day");
    let series = test_issuance::issue_series(
        &url,
        DEPLOYER_KEY,
        day,
        issued_at,
        USD_ISO,
        b'U',
        U256::from(entry),
        load,
        owner,
        &[NOTES],
        &[u32::try_from(world.rpc.chain_id(port).expect("chain ID"))
            .expect("Intex chain ID fits uint32")],
        &[SeriesSpec {
            issuance: *b"USD",
            issuance_currency: USD_ISO,
        }],
    )
    .expect("issue capacity source series")[0];
    let deadline = Instant::now() + Duration::from_secs(180);
    while venue_probes::series_balances(&url, nft, series, owner) != Some((u64::from(NOTES), 0)) {
        assert!(Instant::now() < deadline, "source Intex issuance timed out");
        sleep(Duration::from_millis(500));
    }
    let mut gems = Vec::new();
    for _ in 0..NOTES / GEMS_PER_POSITION {
        // Refresh the previous-day fixture if the long run crosses UTC midnight.
        test_issuance::seed_day_vwaps(&url, DEPLOYER_KEY, USD_ISO, 1, U256::from(entry))
            .expect("seed issuance price");
        let position_index = read(
            &url,
            addresses::GEM_FACTORY_ADDR,
            &eth::IGemFactory::balanceOfCall { owner },
        );
        let parked = send(
            &url,
            addresses::GEM_FACTORY_ADDR,
            &eth::IGemFactory::issueGemPositionCall {
                sourceIntexId: series,
                amount: U256::from(GEMS_PER_POSITION),
            },
        );
        assert_mined_success(&parked, "park capacity batch");
        let position = read(
            &url,
            addresses::GEM_FACTORY_ADDR,
            &eth::IGemFactory::tokenOfOwnerByIndexCall {
                owner,
                index: position_index,
            },
        );
        for _ in 0..GEMS_PER_POSITION {
            // Waiting for each receipt puts equal owner/load GEMs in different blocks.
            let issued = send(
                &url,
                addresses::GEM_FACTORY_ADDR,
                &eth::IGemFactory::issueGemCall {
                    positionId: position,
                    owner,
                    promisLoad: U256::from(load),
                },
            );
            assert_mined_success(&issued, &format!("issue GEM {}", gems.len()));
            let event = single_event::<eth::IGemFactory::GemIssued>(
                &issued.receipt,
                addresses::GEM_FACTORY_ADDR,
            );
            assert_eq!((event.owner, event.promisLoad), (owner, U256::from(load)));
            gems.push(event.gemId);
        }
        assert_eq!(
            read(
                &url,
                addresses::GEM_FACTORY_ADDR,
                &eth::IGemFactory::getPositionCall {
                    positionId: position
                }
            )
            .remainingCapacity,
            U256::ZERO
        );
        if gems.len() % 100 == 0 {
            eprintln!("paynote_capacity phase=issue_gems completed={}", gems.len());
        }
    }
    assert_eq!(gems.len(), NOTES as usize);
    assert_eq!(gems.iter().collect::<BTreeSet<_>>().len(), gems.len());
    let statuses: Vec<_> = gems
        .iter()
        .map(|&gem_id| {
            read(
                &url,
                addresses::GEM_ADDR,
                &eth::IGem::getGemStatusCall { gemId: gem_id },
            )
        })
        .collect();
    let floor = statuses
        .iter()
        .map(|g| g.floorPrice)
        .max()
        .expect("GEM floors");
    let call = statuses
        .iter()
        .map(|g| g.callPrice)
        .min()
        .expect("GEM call prices");
    let quote = floor.checked_add(U256::ONE).expect("qualifying price");
    assert!(quote < call, "qualification must not trigger a call");
    crate::features::price_oracle::publish_controlled_quote(world, quote);
    let now = world
        .rpc
        .latest_block_timestamp(port)
        .expect("qualification timestamp");
    for batch in gems.chunks(DEPOSIT_BATCH) {
        let calls = batch
            .iter()
            .map(|&gem_id| {
                prepared(
                    addresses::GEM_ADDR,
                    &IGemTestArming::backdateGemForTestCall {
                        gemId: gem_id,
                        issuedAt: now.saturating_sub(3 * 86_400),
                    },
                )
            })
            .collect();
        for outcome in
            eth::send_prepared_calls_outcomes(&url, DEPLOYER_KEY, calls).expect("backdate GEMs")
        {
            assert_mined_success(&outcome, "backdate GEM");
        }
    }
    test_issuance::seed_day_vwaps(&url, DEPLOYER_KEY, USD_ISO, 1, quote)
        .expect("seed qualifying day");
    for &gem_id in &gems {
        assert!(
            read(
                &url,
                addresses::GEM_ADDR,
                &eth::IGem::isQualifiedCall { gemId: gem_id }
            ),
            "GEM {gem_id} must qualify"
        );
    }
    gems
}

fn reject_without_mutation(
    world: &World,
    gem_id: U256,
    proof: &[u8],
    nullifier: B256,
    reason: &str,
) {
    let url = world.rpc.url(world.validators.primary_port());
    let owner = crate::world::origin_venue::deployer_address();
    let currency = world.state.settlement_currency.expect("currency");
    let height = head(&url);
    let gem_call = eth::IGem::getGemStatusCall { gemId: gem_id };
    let spent_call = eth::IPayNote::isSpentCall { nullifier };
    let root_call = eth::IPayNote::currentRootCall {};
    let count_call = eth::IPayNote::leafCountCall {};
    let gem = read_at(&url, addresses::GEM_ADDR, &gem_call, height);
    let spent = read_at(&url, addresses::PAYNOTE_ADDR, &spent_call, height);
    let root = read_at(&url, addresses::PAYNOTE_ADDR, &root_call, height);
    let count = read_at(&url, addresses::PAYNOTE_ADDR, &count_call, height);
    let tokens = balances(&url, currency.asset, owner, currency.vault, height);
    let call = eth::IGemFactory::settleGemWithPayNoteCall {
        gemId: gem_id,
        payNoteProof: proof.to_vec().into(),
    };
    assert_eq!(
        eth::read_call_revert_reason_at(&url, addresses::GEM_FACTORY_ADDR, owner, &call, height)
            .expect("specific EVM revert, not an RPC failure"),
        reason
    );
    let outcome = send(&url, addresses::GEM_FACTORY_ADDR, &call);
    assert!(!outcome.success, "{reason}: {}", outcome.receipt);
    assert!(outcome.receipt["logs"]
        .as_array()
        .expect("receipt logs")
        .is_empty());
    let after = head(&url);
    assert_eq!(
        read_at(&url, addresses::GEM_ADDR, &gem_call, after).abi_encode(),
        gem.abi_encode()
    );
    assert_eq!(
        read_at(&url, addresses::PAYNOTE_ADDR, &spent_call, after),
        spent
    );
    assert_eq!(
        read_at(&url, addresses::PAYNOTE_ADDR, &root_call, after),
        root
    );
    assert_eq!(
        read_at(&url, addresses::PAYNOTE_ADDR, &count_call, after),
        count
    );
    assert_eq!(
        balances(&url, currency.asset, owner, currency.vault, after),
        tokens
    );
    eprintln!(
        "paynote_capacity rejected={reason:?} gem_id={gem_id} tx={}",
        outcome.transaction_hash
    );
}

fn read<C: SolCall>(url: &str, address: Address, call: &C) -> C::Return
where
    C::Return: Send + 'static,
{
    eth::read_call_result(url, address, call)
        .unwrap_or_else(|e| panic!("{} at {address}: {e}", C::SIGNATURE))
}

fn read_at<C: SolCall>(url: &str, address: Address, call: &C, height: u64) -> C::Return
where
    C::Return: Send + 'static,
{
    eth::read_call_at_result(url, address, call, height)
        .unwrap_or_else(|e| panic!("{} at {address}, block {height}: {e}", C::SIGNATURE))
}

fn send<C: SolCall>(url: &str, address: Address, call: &C) -> eth::MinedCallOutcome {
    eth::send_call_outcome(url, address, DEPLOYER_KEY, call, None)
        .unwrap_or_else(|e| panic!("{} at {address}: {e:#}", C::SIGNATURE))
}

fn prepared<C: SolCall>(to: Address, call: &C) -> eth::PreparedCall {
    eth::PreparedCall {
        to,
        data: call.abi_encode().into(),
        value: None,
    }
}

fn word(field: &outbe_paynote::Field) -> B256 {
    PayNoteSuit::field_to_b256(field).expect("canonical field")
}

fn head(url: &str) -> u64 {
    eth::block_number(url).expect("canonical head")
}

fn balances(url: &str, asset: Address, owner: Address, vault: Address, height: u64) -> [U256; 4] {
    [
        owner,
        vault,
        addresses::PAYNOTE_ADDR,
        addresses::GEM_FACTORY_ADDR,
    ]
    .map(|account| read_at(url, asset, &ITestToken::balanceOfCall { account }, height))
}

fn finalize(world: &World, outcome: &eth::MinedCallOutcome) -> u64 {
    world
        .rpc
        .finalize_outcome(
            &crate::world::rpc::TxOutcome {
                transaction_hash: outcome.transaction_hash.clone(),
                success: outcome.success,
                receipt: outcome.receipt.clone(),
            },
            &world.validators.committee_ports(),
            120,
        )
        .expect("identical finalized checkpoint on every validator")
        .height
}

fn matching_logs<E: SolEvent>(receipt: &Value, emitter: Address) -> Vec<&Value> {
    receipt["logs"]
        .as_array()
        .expect("receipt logs")
        .iter()
        .filter(|log| {
            log["address"] == format!("{emitter:#x}")
                && log["topics"][0] == format!("{:#x}", E::SIGNATURE_HASH)
        })
        .collect()
}

fn event_count<E: SolEvent>(receipt: &Value, emitter: Address) -> usize {
    matching_logs::<E>(receipt, emitter).len()
}

fn single_event<E: SolEvent>(receipt: &Value, emitter: Address) -> E {
    let logs = matching_logs::<E>(receipt, emitter);
    assert_eq!(logs.len(), 1, "exactly one {} event", E::SIGNATURE);
    let topics: Vec<B256> =
        serde_json::from_value(logs[0]["topics"].clone()).expect("event topics");
    let data: alloy_primitives::Bytes =
        serde_json::from_value(logs[0]["data"].clone()).expect("event data");
    E::decode_raw_log_validate(topics, &data).expect("canonical event")
}
