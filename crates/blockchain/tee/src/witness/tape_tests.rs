use super::tape::*;
use crate::nod_mine::{IssueNodFromTributeOutcomeV1, MineEncryptedNodResultV1};
use crate::protocol::{
    EnclaveResponse, FidelityOpOutcome, FidelityQueryResult, GratisOpResult, GratisOpStatus,
    MineGratisFromPromisResultV1, MineGratisFromPromisStatusV1, PromisOpResult, PromisOpStatus,
};
use crate::tribute_day::LysisFractionsResultV1;
use alloy_primitives::{B256, U256};
use outbe_primitives::{
    nod_encryption::{EncryptedNodGroupLoadV1, EncryptedNodV1, NodTermsV1},
    time::{EmissionDate, WorldwideDay},
    tribute_day_encryption::{
        EncryptedTributeDayAmountV2, EncryptedTributeDayFractionsV1, EncryptedTributeLeagueAmountV1,
    },
    tribute_encryption::{EncryptedTributeV2, TributeAmountsV2},
    wwd_entity_id::WwdEntityId,
};

fn entries() -> Vec<EnclaveWitnessEntryV1> {
    let fidelity = FidelityOpOutcome {
        new_blob: vec![1],
        qualified_start_initialized: Some(2),
        league: 3,
    };
    let nod = EncryptedNodV1 {
        terms: NodTermsV1 {
            chain_id: 1,
            nod_id: U256::ZERO,
            owner: Default::default(),
            emission_day: 20261028,
            league_id: 2,
            entry_price_minor: U256::from(3),
            issuance_currency: 4,
            reference_currency: 5,
        },
        encryption_binding: B256::ZERO,
        encrypted_gratis_amount: vec![7],
    };
    let tribute = EncryptedTributeV2 {
        context: outbe_primitives::tribute_encryption::TributeContextV2 {
            chain_id: 1,
            tribute_id: WwdEntityId::from_day_and_digest(WorldwideDay::new(20261028), B256::ZERO),
            owner: Default::default(),
            worldwide_day: WorldwideDay::new(20261028),
            issuance_currency: 2,
            reference_currency: 3,
            tribute_price_minor: U256::from(4),
            offer_input_hash: B256::ZERO,
        },
        encrypted_amounts: vec![9],
    };
    let tag = vec![0xAA; 64];
    vec![
        EnclaveWitnessEntryV1::GratisOpApplied {
            result: Box::new(GratisOpResult {
                status: GratisOpStatus::Applied,
                new_balance: vec![1],
                new_pledged: vec![2],
                event_amount: U256::from(3),
                encrypted_event_amount: vec![4],
                next_op_nonce: 4,
                fidelity: Some(fidelity.clone()),
                inputs_canonical_hash: B256::ZERO,
                attestation_tag: tag.clone(),
            }),
        },
        EnclaveWitnessEntryV1::PromisOpApplied {
            result: Box::new(PromisOpResult {
                status: PromisOpStatus::Rejected { reason: "r".into() },
                new_balance: vec![2],
                event_amount: U256::from(3),
                encrypted_event_amount: vec![4],
                next_op_nonce: 4,
                inputs_canonical_hash: B256::ZERO,
                attestation_tag: tag.clone(),
            }),
        },
        EnclaveWitnessEntryV1::EncryptedNodMinedV1 {
            result: Box::new(MineEncryptedNodResultV1 {
                new_balance: vec![1],
                encrypted_event_amount: vec![5],
                next_op_nonce: 2,
                fidelity: fidelity.clone(),
                inputs_canonical_hash: B256::ZERO,
                attestation_tag: tag.clone(),
            }),
        },
        EnclaveWitnessEntryV1::EncryptedNodMintRejectedV1 {
            reason: "reject".into(),
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::NodAmountReadV1 {
            amount: U256::from(4),
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::NodEncryptedForTributeV1 {
            nod,
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::EncryptedTributeOfferBatchV2 {
            results: vec![crate::tribute_v2::EncryptedTributeOfferResultV2 {
                token_id: B256::ZERO,
                tribute: Some(tribute.clone()),
                su_hashes: vec!["a".into()],
                wallet_addresses: vec![],
                sra_addresses: vec![],
                zk_expected_hashes: None,
                status: crate::protocol::TributeOfferStatus::Created,
            }],
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::TributeAmountsReadV2 {
            amounts: vec![TributeAmountsV2 {
                issuance_amount_minor: U256::from(1),
                nominal_amount_minor: U256::from(2),
            }],
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::TributeDayOpAppliedV2 {
            record: EncryptedTributeDayAmountV2 {
                chain_id: 1,
                worldwide_day: WorldwideDay::new(20261028),
                frozen: true,
                operation_hash: B256::ZERO,
                encrypted_amount: vec![3],
            },
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::TributeDayAmountReadV2 {
            amount: U256::from(2),
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::FidelityLeaguesSnapshotted {
            leagues: vec![crate::protocol::FidelityLeagueEntry {
                owner: Default::default(),
                league: 2,
                rcfi: U256::from(3),
            }],
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::FidelityIndexQueried {
            result: Box::new(FidelityQueryResult {
                rcfi: U256::from(1),
                efficiency: U256::from(2),
                league: 3,
                inputs_canonical_hash: B256::ZERO,
                attestation_tag: tag.clone(),
            }),
        },
        EnclaveWitnessEntryV1::FidelityQueryRejected {
            reason: "reject".into(),
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::DcapVerificationFinishedV1 {
            request_hash: B256::ZERO,
            outcome: vec![1],
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::DcapOnboardingVerificationFinishedV1 {
            request_hash: B256::ZERO,
            outcome: vec![1],
            onboarding_artifact: vec![2],
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::GramineDirectDevOnboardingArtifactPreparedV2 {
            request_hash: B256::ZERO,
            onboarding_artifact: vec![2],
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::TributeLeagueOpAppliedV1 {
            record: EncryptedTributeLeagueAmountV1 {
                chain_id: 1,
                worldwide_day: WorldwideDay::new(20261028),
                league: 100,
                operation_hash: B256::ZERO,
                encrypted_amount: vec![4],
            },
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::TributeLeagueAmountReadV1 {
            amount: U256::from(5),
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::LysisFractionsExecutedV1 {
            result: Box::new(LysisFractionsResultV1 {
                day_fractions: EncryptedTributeDayFractionsV1 {
                    chain_id: 1,
                    worldwide_day: WorldwideDay::new(20261027),
                    emission_day: EmissionDate::new(20261028),
                    completed_at: 10,
                    operation_hash: B256::ZERO,
                    encrypted_fractions: vec![6],
                },
                total_nominal_minor: U256::from(7),
                gratis_demand_minor: U256::from(2),
                lysis_limit_minor: U256::from(2),
                allocated_minor: U256::from(8),
                zero_load_tribute_ids: vec![],
                series_loads: vec![],
            }),
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::TributeDayFractionsReadV1 {
            fractions: std::collections::BTreeMap::from([(100, 500_000)]),
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::NodIssuedFromTributeV1 {
            outcome: IssueNodFromTributeOutcomeV1::ZeroSettlementCost,
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::NodGroupLoadOpAppliedV1 {
            record: EncryptedNodGroupLoadV1 {
                chain_id: 1,
                scope: B256::ZERO,
                operation_hash: B256::ZERO,
                encrypted_load: vec![9],
            },
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::NodGroupLoadReadV1 {
            amount: U256::from(10),
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::NodQuoteEncryptedV1 {
            encrypted_payment: vec![11],
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::GratisMinedFromPromisV1 {
            result: Box::new(MineGratisFromPromisResultV1 {
                status: MineGratisFromPromisStatusV1::Applied,
                new_promis_balance: vec![12],
                encrypted_promis_event_amount: vec![13],
                next_promis_op_nonce: 1,
                new_gratis_balance: vec![14],
                encrypted_gratis_event_amount: vec![15],
                next_gratis_op_nonce: 1,
                fidelity: Some(fidelity),
                inputs_canonical_hash: B256::ZERO,
                attestation_tag: tag,
            }),
        },
    ]
}

#[test]
fn all_frozen_variants_round_trip_conversion_and_serialize_with_indices_zero_through_fifteen() {
    for (index, entry) in entries().into_iter().enumerate() {
        let response = EnclaveResponse::from(entry.clone());
        let converted = EnclaveWitnessEntryV1::try_from(response.clone()).unwrap();
        assert_eq!(converted, entry);
        assert_eq!(EnclaveResponse::from(converted), response);
        let encoded = postcard::to_allocvec(&entry).unwrap();
        assert_eq!(encoded[0], index as u8, "variant index {index}");
    }
}

#[test]
fn semantic_equality_ignores_only_attestation_tag() {
    for entry in entries() {
        let mut other = entry.clone();
        match &mut other {
            EnclaveWitnessEntryV1::GratisOpApplied { result } => result.attestation_tag.push(1),
            EnclaveWitnessEntryV1::PromisOpApplied { result } => result.attestation_tag.push(1),
            EnclaveWitnessEntryV1::EncryptedNodMinedV1 { result } => result.attestation_tag.push(1),
            EnclaveWitnessEntryV1::GratisMinedFromPromisV1 { result } => {
                result.attestation_tag.push(1)
            }
            EnclaveWitnessEntryV1::EncryptedNodMintRejectedV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::NodAmountReadV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::NodEncryptedForTributeV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::EncryptedTributeOfferBatchV2 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::TributeAmountsReadV2 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::TributeDayOpAppliedV2 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::TributeDayAmountReadV2 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::FidelityLeaguesSnapshotted {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::FidelityQueryRejected {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::DcapVerificationFinishedV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::DcapOnboardingVerificationFinishedV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::GramineDirectDevOnboardingArtifactPreparedV2 {
                attestation_tag,
                ..
            }
            | EnclaveWitnessEntryV1::TributeLeagueOpAppliedV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::TributeLeagueAmountReadV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::LysisFractionsExecutedV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::TributeDayFractionsReadV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::NodIssuedFromTributeV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::NodGroupLoadOpAppliedV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::NodGroupLoadReadV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::NodQuoteEncryptedV1 {
                attestation_tag, ..
            } => attestation_tag.push(1),
            EnclaveWitnessEntryV1::FidelityIndexQueried { result } => {
                result.attestation_tag.push(1)
            }
        }
        assert!(entry.semantic_eq_ignoring_attestation_tag(&other));
    }
    let variants = entries();
    for (i, entry) in variants.iter().enumerate() {
        assert!(!entry.semantic_eq_ignoring_attestation_tag(&variants[(i + 1) % variants.len()]));
    }
}

fn mutate_commitment(entry: &mut EnclaveWitnessEntryV1) {
    let changed = B256::repeat_byte(1);
    match entry {
        EnclaveWitnessEntryV1::GratisOpApplied { result } => {
            result.inputs_canonical_hash = changed;
        }
        EnclaveWitnessEntryV1::PromisOpApplied { result } => {
            result.inputs_canonical_hash = changed;
        }
        EnclaveWitnessEntryV1::EncryptedNodMinedV1 { result } => {
            result.inputs_canonical_hash = changed;
        }
        EnclaveWitnessEntryV1::GratisMinedFromPromisV1 { result } => {
            result.inputs_canonical_hash = changed;
        }
        EnclaveWitnessEntryV1::EncryptedNodMintRejectedV1 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::NodAmountReadV1 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::NodEncryptedForTributeV1 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::EncryptedTributeOfferBatchV2 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::TributeAmountsReadV2 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::TributeDayOpAppliedV2 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::TributeDayAmountReadV2 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::FidelityLeaguesSnapshotted {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::FidelityQueryRejected {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::TributeLeagueOpAppliedV1 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::TributeLeagueAmountReadV1 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::LysisFractionsExecutedV1 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::TributeDayFractionsReadV1 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::NodIssuedFromTributeV1 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::NodGroupLoadOpAppliedV1 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::NodGroupLoadReadV1 {
            inputs_canonical_hash,
            ..
        }
        | EnclaveWitnessEntryV1::NodQuoteEncryptedV1 {
            inputs_canonical_hash,
            ..
        } => {
            *inputs_canonical_hash = changed;
        }
        EnclaveWitnessEntryV1::FidelityIndexQueried { result } => {
            result.inputs_canonical_hash = changed;
        }
        EnclaveWitnessEntryV1::DcapVerificationFinishedV1 { request_hash, .. }
        | EnclaveWitnessEntryV1::DcapOnboardingVerificationFinishedV1 { request_hash, .. }
        | EnclaveWitnessEntryV1::GramineDirectDevOnboardingArtifactPreparedV2 {
            request_hash,
            ..
        } => {
            *request_hash = changed;
        }
    }
}

#[test]
fn semantic_equality_compares_commitments_in_all_sixteen_variants() {
    for entry in entries() {
        let mut changed = entry.clone();
        mutate_commitment(&mut changed);
        assert!(
            !entry.semantic_eq_ignoring_attestation_tag(&changed),
            "{entry:?}"
        );
    }
}

#[test]
fn semantic_equality_compares_nested_ciphertext_and_fidelity_fields() {
    let gratis = entries().remove(0);
    let mut changed_gratis = gratis.clone();
    if let EnclaveWitnessEntryV1::GratisOpApplied { result } = &mut changed_gratis {
        result.new_balance.push(0xFF);
        result.fidelity.as_mut().unwrap().new_blob.push(0xEE);
    }
    assert!(!gratis.semantic_eq_ignoring_attestation_tag(&changed_gratis));

    let mine = entries().remove(2);
    let mut changed_mine = mine.clone();
    if let EnclaveWitnessEntryV1::EncryptedNodMinedV1 { result } = &mut changed_mine {
        result.new_balance.push(0xFF);
        result.fidelity.new_blob.push(0xEE);
    }
    assert!(!mine.semantic_eq_ignoring_attestation_tag(&changed_mine));
}

#[test]
fn unsupported_responses_are_not_witness_entries() {
    assert!(EnclaveWitnessEntryV1::try_from(EnclaveResponse::Ack).is_err());
    assert!(EnclaveWitnessEntryV1::try_from(EnclaveResponse::Error {
        message: "err".into()
    })
    .is_err());
}
