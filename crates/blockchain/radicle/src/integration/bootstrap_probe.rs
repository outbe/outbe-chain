//! Genesis and native Project must accept the same literal description byte range.

use outbe_primitives::radicle_genesis::GovernmentsDescriptor;
use radicle::{git, identity::Project};

#[test]
fn genesis_description_range_matches_native_project_range() {
    for description in [
        String::new(),
        "x".repeat(255),
        "x".repeat(256),
        "я".repeat(128),
        format!("{}x", "я".repeat(127)),
        "x".repeat(257),
        format!("{}x", "я".repeat(128)),
        "x".repeat(1024),
    ] {
        let length = description.len();
        let descriptor = GovernmentsDescriptor {
            chain_id: 2026,
            genesis_timestamp: 1700000000,
            description,
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
        assert_eq!(genesis.is_ok(), length <= 256);
        assert_eq!(native.is_ok(), length <= 256);
        if let Ok(project) = native {
            assert_eq!(project.description(), descriptor.description);
            assert_eq!(
                serde_json::from_slice::<Project>(&serde_json::to_vec(&project).unwrap()).unwrap(),
                project
            );
        }
        if let Ok(encoded) = genesis {
            assert_eq!(
                GovernmentsDescriptor::from_bytes(&encoded).unwrap(),
                descriptor
            );
        }
    }
}
