//! Static storage layout descriptions emitted by `#[contract]` and
//! `#[storage_record]`.
//!
//! The macros compute every slot at compile time. These tables expose the same
//! numbers to tests and tools, so a layout review or a snapshot test reads the
//! layout the code uses and does not parse the source.

use std::fmt::Write as _;

/// One field of a `#[contract]` storage struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldLayout {
    /// Rust field name.
    pub name: &'static str,
    /// First slot of the field.
    pub slot: u64,
    /// Number of consecutive slots the field reserves from `slot`.
    pub slots: u64,
    /// Field type as written in the struct, without the DSL module path.
    pub ty: &'static str,
}

/// One non-key attribute of a `#[storage_record]`.
///
/// A record stored under `Map<K, Record>` at base slot `B` keeps this
/// attribute in the mapping at slot `B + slot_offset`, keyed by the record key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordFieldLayout {
    /// Rust field name.
    pub name: &'static str,
    /// Offset of the attribute's mapping from the record's base slot.
    pub slot_offset: u64,
    /// Number of mapping slots the attribute reserves (`Optional` takes two).
    pub slots: u64,
    /// First byte of the value inside its word, counted from the low end.
    /// Always `0` for an attribute that has its own word.
    pub byte_offset: u64,
    /// Bytes the value takes inside its word. `32` for an attribute that has
    /// its own word.
    pub bytes: u64,
    /// Field type as written in the struct.
    pub ty: &'static str,
}

/// Renders a contract layout as a Markdown table, in slot order.
pub fn render_contract(name: &str, fields: &[FieldLayout]) -> String {
    let mut out = format!("## {name}\n\n| slot | slots | field | type |\n|---:|---:|---|---|\n");
    for f in fields {
        let _ = writeln!(
            out,
            "| {} | {} | `{}` | `{}` |",
            f.slot, f.slots, f.name, f.ty
        );
    }
    out
}

/// Renders a record layout as a Markdown table, in slot order.
pub fn render_record(name: &str, total_slots: u64, fields: &[RecordFieldLayout]) -> String {
    let mut out = format!(
        "## {name} ({total_slots} slots)\n\n\
         | offset | slots | bytes | field | type |\n|---:|---:|---|---|---|\n"
    );
    for f in fields {
        let bytes = if f.bytes == 32 {
            "word".to_string()
        } else {
            format!("{}..{}", f.byte_offset, f.byte_offset + f.bytes)
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | `{}` | `{}` |",
            f.slot_offset, f.slots, bytes, f.name, f.ty
        );
    }
    out
}

/// Compares `rendered` with the snapshot file at `path`.
///
/// With `UPDATE_STORAGE_LAYOUT=1` the call writes `rendered` to `path` and
/// passes. Otherwise a missing or different file is an error that names the
/// command to update it.
pub fn check_snapshot(path: &std::path::Path, rendered: &str) -> Result<(), String> {
    if std::env::var_os("UPDATE_STORAGE_LAYOUT").is_some_and(|value| value == "1") {
        return std::fs::write(path, rendered)
            .map_err(|e| format!("write {}: {e}", path.display()));
    }
    let current = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "read {}: {e}. Run the test with UPDATE_STORAGE_LAYOUT=1 to create it.",
            path.display()
        )
    })?;
    if current == rendered {
        return Ok(());
    }
    Err(format!(
        "storage layout differs from {}. If the change is intended, run the test with \
         UPDATE_STORAGE_LAYOUT=1 and commit the file.\n\n--- file\n{current}\n--- code\n{rendered}",
        path.display()
    ))
}
