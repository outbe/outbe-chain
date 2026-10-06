use super::*;

fn legacy_json() -> Result<Vec<String>> {
    let context = serde_json::to_string(&tests::context(Path::new("/legacy")))?;
    let submission = serde_json::to_string(&tests::submission())?;
    let root = B256::repeat_byte(6);
    let offer = B256::repeat_byte(7);
    let proof = B256::repeat_byte(8);
    let block = B256::repeat_byte(9);
    let transaction = tests::submission().relay_variants[0].transaction_hash;
    let material = format!(
        r#","sealed_root_hash":"{root}","resident_offer_public":"{offer}","proof_hash":"{proof}""#
    );
    let prepared = format!(r#"{material},"submission":{submission}"#);
    let finalized = format!(r#"{prepared},"finalized_height":91,"finalized_hash":"{block}""#);
    Ok([
        ("candidatePrepared", String::new()),
        ("keyProvisioned", format!(r#","sealed_root_hash":"{root}""#)),
        ("candidateKeyReady", material),
        ("submissionPrepared", prepared.clone()),
        ("submitted", format!(r#"{prepared},"submitted_at_finalized_height":90,"transaction_hashes":["{transaction}"]"#)),
        ("finalized", finalized.clone()),
        ("promoted", finalized),
        ("terminalMissedCutoff", r#","finalized_height":100,"activation_height":100"#.to_owned()),
    ].into_iter().map(|(state, tail)| {
        format!(r#"{{"state":"{state}","context":{context}{tail}}}"#)
    }).collect())
}

fn legacy_states() -> Result<Vec<UpgradeJournalStateV1>> {
    legacy_json()?
        .iter()
        .map(|json| serde_json::from_str(json).map_err(Into::into))
        .collect()
}

#[test]
fn legacy_v1_all_states_keep_exact_wire_bytes_and_validate() {
    for json in legacy_json().unwrap() {
        let state: UpgradeJournalStateV1 = serde_json::from_str(&json).unwrap();
        state.validate().unwrap();
        assert_eq!(serde_json::to_string(&state).unwrap(), json);
    }
}

#[test]
fn legacy_v1_all_states_load_without_rewriting_the_journal() {
    let directory = tempfile::tempdir().unwrap();
    let guard = UpgradeJournalGuardV1::acquire(directory.path()).unwrap();
    let path = directory.path().join(DIRECTORY).join("journal.json");
    for json in legacy_json().unwrap() {
        let bytes = format!(r#"{{"version":1,"generation":7,"lifecycle":{json}}}"#);
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(FILE_MODE)).unwrap();
        let snapshot = guard.load().unwrap().unwrap();
        assert_eq!(serde_json::to_string(&snapshot).unwrap(), bytes);
        assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
    }
}

#[test]
fn legacy_v1_rejects_unknown_missing_and_duplicate_fields() {
    for json in legacy_json().unwrap() {
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        for field in value
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| key.as_str() != "state")
        {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<UpgradeJournalStateV1>(missing).is_err(),
                "missing {field}"
            );
            let duplicate = format!(
                r#"{},"{field}":{}}}"#,
                &json[..json.len() - 1],
                value[field]
            );
            assert!(
                serde_json::from_str::<UpgradeJournalStateV1>(&duplicate).is_err(),
                "duplicate {field}"
            );
        }
        for field in ["unexpected", "security"] {
            let mut unknown = value.clone();
            unknown[field] = serde_json::json!({});
            assert!(serde_json::from_value::<UpgradeJournalStateV1>(unknown).is_err());
        }
        let mut unknown = value;
        unknown["context"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<UpgradeJournalStateV1>(unknown).is_err());
    }
}

#[test]
fn legacy_v1_root_copied_alias_normalizes_without_security_payload() {
    let canonical = &legacy_json().unwrap()[1];
    let alias = canonical.replace("keyProvisioned", "rootCopied");
    let state: UpgradeJournalStateV1 = serde_json::from_str(&alias).unwrap();
    state.validate().unwrap();
    assert_eq!(serde_json::to_string(&state).unwrap(), *canonical);
}

#[test]
fn legacy_v1_transition_matrix_preserves_all_recovery_and_terminal_edges() {
    let allowed = [
        (0, 1),
        (0, 7),
        (1, 2),
        (1, 7),
        (2, 1),
        (2, 3),
        (2, 7),
        (3, 1),
        (3, 4),
        (3, 7),
        (4, 1),
        (4, 4),
        (4, 5),
        (4, 7),
        (5, 6),
        (6, 6),
        (7, 7),
    ];
    let states = legacy_states().unwrap();
    for (from, current) in states.iter().enumerate() {
        for (to, next) in states.iter().enumerate() {
            assert_eq!(
                validate_checkpoint_transition(current, next).is_ok(),
                allowed.contains(&(from, to)),
                "{from} -> {to}"
            );
        }
    }
}

#[test]
fn legacy_v1_transition_rejects_each_changed_security_commitment() {
    let states = legacy_states().unwrap();
    for (from, to) in [(1, 2), (2, 3), (3, 4), (4, 4), (4, 5), (5, 6), (6, 6)] {
        let fields = if from == 1 {
            &["sealed_root_hash"][..]
        } else {
            &["sealed_root_hash", "resident_offer_public", "proof_hash"][..]
        };
        for field in fields {
            let mut value = serde_json::to_value(&states[to]).unwrap();
            value[*field] = serde_json::json!(B256::repeat_byte(10));
            let next: UpgradeJournalStateV1 = serde_json::from_value(value).unwrap();
            assert_eq!(
                validate_checkpoint_transition(&states[from], &next)
                    .unwrap_err()
                    .to_string(),
                "upgrade security material changed across checkpoints"
            );
        }
    }
    let mut wrong_root = serde_json::to_value(&states[1]).unwrap();
    wrong_root["sealed_root_hash"] = serde_json::json!(B256::repeat_byte(10));
    let next = serde_json::from_value(wrong_root).unwrap();
    for from in [2, 3, 4] {
        assert_eq!(
            validate_checkpoint_transition(&states[from], &next)
                .unwrap_err()
                .to_string(),
            "expired submission recovery changed the sealed root"
        );
    }
}

#[test]
fn legacy_v1_deserialization_and_validation_keep_distinct_error_order() {
    for json in legacy_json().unwrap().into_iter().skip(2).take(5) {
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["sealed_root_hash"] = serde_json::json!(B256::ZERO);
        value["resident_offer_public"] = serde_json::json!(B256::ZERO);
        value["proof_hash"] = serde_json::json!(B256::ZERO);
        let state: UpgradeJournalStateV1 = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            state.validate().unwrap_err().to_string(),
            "upgrade checkpoint has a zero sealed-root hash"
        );
        value["sealed_root_hash"] = serde_json::json!(B256::repeat_byte(6));
        let state: UpgradeJournalStateV1 = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            state.validate().unwrap_err().to_string(),
            "key-ready checkpoint has a zero offer key or proof hash"
        );
        value["context"]["activationHeight"] = serde_json::json!(0);
        let state: UpgradeJournalStateV1 = serde_json::from_value(value).unwrap();
        let mut context = tests::context(Path::new("/legacy"));
        context.activation_height = 0;
        assert_eq!(
            state.validate().unwrap_err().to_string(),
            context.validate().unwrap_err().to_string()
        );
    }
}
