//! Network-bound SGX release bundle preparation, signing and verification.

mod spec;
pub use spec::{BundleSpec, SgxPolicy, SgxReleaseNetwork};
use spec::{EXCLUDED_BUNDLE_FILES, GITHUB_ACTIONS_OIDC_ISSUER, REQUIRED_BUNDLE_FILES};

mod manifest;
pub use manifest::{
    build_release_manifest_candidate, canonical_json, BundleFile, BundleManifest, GramineIdentity,
    ManifestSource, Measurements, OciBuildEvidence, OciDescriptor, Sha256Digest, SourceIdentity,
    VerifiedReleaseInputs,
};
use manifest::{build_release_manifest_from_evidence, release_platform};

mod signatures;
use signatures::{verified_cosign_attestation, verify_cosign_image_signature};

mod evidence;
use evidence::{
    passed_gate, passed_gate_many, require_bundle_measurement_binding, require_bundle_network,
    require_evidence_result, require_fresh_dcap_hardware_evidence,
    require_seeded_genesis_release_evidence, verify_processor_dcap_archive,
};

mod bundle;
pub use bundle::{
    build_bundle_manifest, compare_unsigned_trees, parse_oci_descriptor, parse_sigstruct_view,
    verify_signed_bundle, ComparisonEvidence,
};
use bundle::{
    read_measured_network_descriptor, require_measured_network_descriptor, sigstruct_date,
    tree_entries, TreeEntry,
};

mod archive;
pub use archive::write_deterministic_bundle_archive;
use archive::{validate_archive_member_path, verify_bundle_archive};

mod files;
use files::{
    file_artifact, file_digest, is_lower_hex, normalize_tree_mtime, read_canonical_json,
    require_nonempty_regular_file, verify_checksums, write_canonical, write_checksums,
    write_new_file,
};

mod checkout;
pub use checkout::repository_root;
use checkout::{
    absolute_path, create_empty_output, read_elf_identity, require_clean_source,
    require_release_checkout, validate_signing_key, validate_source_identity,
};

mod toolchain;
use toolchain::{
    build_project_toolchain_image, container_adapter, docker_command, run_output, run_status,
};

mod commands;
pub use commands::{
    archive, build_image, compare, finalize_genesis, finalize_release_manifest,
    normalize_cosign_json_output, prepare, sign, verify, verify_with_genesis,
};
