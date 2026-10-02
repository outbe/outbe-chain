//! Diagnostics through the same expansion functions used by the public macros.

use super::*;

fn contract_error(input: &str) -> String {
    generate_contract(syn::parse_str(input).unwrap(), syn::parse_str("").unwrap())
        .unwrap_err()
        .to_string()
}

fn record_error(input: &str, config: &str) -> String {
    generate_storage_record(
        syn::parse_str(input).unwrap(),
        syn::parse_str(config).unwrap(),
    )
    .unwrap_err()
    .to_string()
}

#[test]
fn contract_shape_and_reserved_field_diagnostics_are_preserved() {
    for (input, expected) in [
        ("enum C { A }", "only structs are supported"),
        (
            "struct C(u64);",
            "only structs with named fields are supported",
        ),
        (
            "struct C { address: u64 }",
            "field name `address` is reserved - generated automatically",
        ),
        (
            "struct C { storage: u64 }",
            "field name `storage` is reserved - generated automatically",
        ),
    ] {
        assert_eq!(contract_error(input), expected);
    }
}

#[test]
fn unsupported_field_markers_keep_their_diagnostic() {
    let expected = "unsupported key in #[attribute(...)]";
    assert_eq!(
        contract_error("struct C { #[attribute(bad=1)] value: Value<u64> }"),
        expected
    );
    assert_eq!(
        record_error(
            "struct R { #[key] id: u64, #[attribute(bad=1)] active: u64 }",
            "exists_field=active"
        ),
        expected
    );
}

#[test]
fn record_key_and_config_errors_precede_field_validation() {
    assert_eq!(
        record_error("struct R { value: Option<String> }", ""),
        "#[storage_record] requires exactly one #[key] field"
    );
    assert_eq!(
        record_error(
            "struct R { #[key] id: u64, #[key] other: u64 }",
            "exists_field=other"
        ),
        "#[storage_record] requires exactly one #[key] field"
    );
    assert_eq!(
        record_error("struct R { #[key] id: u64, value: Option<String> }", ""),
        "#[storage_record(...)] requires `exists_field = field_name`"
    );
}

#[test]
fn record_layout_errors_precede_dynamic_and_existence_validation() {
    assert_eq!(
        record_error(
            "struct R { #[key] id: u64, value: Option<String>, old: Deprecated }",
            "exists_field=absent"
        ),
        "Deprecated<T> requires one type arg"
    );
    for ty in ["Option<String>", "Optional<Vec<u8>>"] {
        assert_eq!(
            record_error(
                &format!("struct R {{ #[key] id: u64, value: {ty} }}"),
                "exists_field=absent"
            ),
            "optional dynamic String or Vec<u8> record fields are not supported"
        );
    }
}

#[test]
fn record_existence_field_must_be_a_non_key_field() {
    for field in ["id", "absent"] {
        assert_eq!(
            record_error(
                "struct R { #[key] id: u64, value: u64 }",
                &format!("exists_field={field}")
            ),
            format!("exists_field `{field}` not found among non-key fields")
        );
    }
}
