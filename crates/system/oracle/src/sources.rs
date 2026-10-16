//! Feeder sources compiled into the node, one file per protocol version.
//!
//! A file takes the protocol version that introduces it. The node serves the
//! file with the highest version that is not above the active one, so every
//! validator switches sources in the block that activates the version. A
//! released file never changes: a new source set is a new file.

use outbe_update::{encode_protocol_version, ProtocolVersion};

/// `(version, TOML)` in ascending version order. The first entry is the
/// baseline for a chain with no activated upgrade.
const SOURCES: &[(ProtocolVersion, &str)] = &[(
    encode_protocol_version(0, 0),
    include_str!("../sources/0.0.toml"),
)];

/// Returns the feeder sources TOML for the `active` protocol version.
pub fn sources_for(active: ProtocolVersion) -> &'static str {
    select(SOURCES, active)
}

fn select(table: &[(ProtocolVersion, &'static str)], active: ProtocolVersion) -> &'static str {
    table
        .iter()
        .rev()
        .find(|(version, _)| *version <= active)
        .map(|(_, toml)| *toml)
        .expect("the baseline entry has version zero")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_starts_at_zero_and_ascends() {
        assert_eq!(SOURCES[0].0, ProtocolVersion::ZERO);
        assert!(SOURCES.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn selects_the_newest_file_not_above_the_active_version() {
        let table = [
            (encode_protocol_version(0, 0), "base"),
            (encode_protocol_version(1, 4), "v1.4"),
        ];
        assert_eq!(select(&table, ProtocolVersion::ZERO), "base");
        assert_eq!(select(&table, encode_protocol_version(1, 3)), "base");
        assert_eq!(select(&table, encode_protocol_version(1, 4)), "v1.4");
        assert_eq!(select(&table, encode_protocol_version(2, 0)), "v1.4");
    }
}
