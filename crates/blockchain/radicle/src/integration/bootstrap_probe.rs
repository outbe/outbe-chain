//! D3 diagnostic only: passing this test confirms the incompatible accepted input ranges.

use outbe_primitives::radicle_genesis::GovernmentsDescriptor;
use radicle::{git, identity::Project};

#[test]
fn genesis_description_range_exceeds_native_project_range() {
    for length in [0, 255, 256, 1024, 1025] {
        let descriptor = GovernmentsDescriptor {
            chain_id: 2026,
            genesis_timestamp: 1700000000,
            description: "x".repeat(length),
            initial_tree_oid: "0x4b825dc642cb6eb9a060e54bf8d69288fbee4904"
                .parse()
                .unwrap(),
            initial_content_manifest_sha256:
                "0x4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
                    .parse()
                    .unwrap(),
        };
        let genesis = descriptor.to_bytes();
        let native = Project::new(
            "governments".parse().unwrap(),
            descriptor.description.clone(),
            git::fmt::refname!("main"),
        );
        println!(
            "description_bytes={length} genesis_accepts={} native_accepts={} native_error={:?}",
            genesis.is_ok(),
            native.is_ok(),
            native.as_ref().err()
        );
        assert_eq!(genesis.is_ok(), length <= 1024);
        assert_eq!(native.is_ok(), length <= 255);
        if let Ok(encoded) = genesis {
            assert_eq!(
                GovernmentsDescriptor::from_bytes(&encoded).unwrap(),
                descriptor
            );
        }
    }
}
