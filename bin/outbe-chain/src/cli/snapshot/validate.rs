use crate::snapshot::validation::{
    report::ValidationReport,
    run::{report_protected_paths, validate_snapshot, ValidationInputs},
};
use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(clap::Args)]
pub(crate) struct ValidateArgs {
    /// Checks to run: all, or comma-separated files,provenance,headers,evm,ce,bodies,ocomp.
    #[arg(long, default_value = "all")]
    pub checks: String,
    #[arg(long)]
    pub manifest: Option<PathBuf>,
    #[arg(long)]
    pub signature: Option<PathBuf>,
    #[arg(long)]
    pub archive: Option<PathBuf>,
    /// Expected creator compressed secp256k1 public key in hexadecimal.
    #[arg(long, value_parser = parse_signer)]
    pub expected_signer: Option<[u8; 33]>,
    /// New JSON report outside native data and configuration paths.
    #[arg(long)]
    pub report: Option<PathBuf>,
    /// Ordinary node options after --. All native writers must remain stopped.
    #[arg(last = true)]
    pub node_args: Vec<OsString>,
}

fn parse_signer(value: &str) -> Result<[u8; 33], String> {
    let mut public_key = [0_u8; 33];
    hex::decode_to_slice(value.strip_prefix("0x").unwrap_or(value), &mut public_key)
        .map_err(|error| format!("expected 33-byte compressed public key: {error}"))?;
    Ok(public_key)
}

fn write_report(path: &Path, report: &ValidationReport) -> eyre::Result<()> {
    use eyre::WrapErr as _;
    outbe_snapshot::layout::validate_layout(&[], &report.protected_paths, &[path.to_path_buf()])?;
    let mut output = outbe_snapshot::fs::PendingArchive::new(path)
        .wrap_err_with(|| format!("create validation report {}", path.display()))?;
    serde_json::to_writer_pretty(&mut output.file, report)?;
    output.file.write_all(b"\n")?;
    output
        .publish()
        .wrap_err_with(|| format!("publish validation report {}", path.display()))?;
    Ok(())
}

pub(super) fn run(args: ValidateArgs) -> eyre::Result<()> {
    let inputs = ValidationInputs {
        checks: args.checks,
        manifest: args.manifest,
        signature: args.signature,
        archive: args.archive,
        expected_signer: args.expected_signer,
    };
    // The validation engine creates its own bounded work under this external
    // disposable directory. No scratch is placed in the copied native stores.
    let scratch = tempfile::tempdir()?;
    // Resolve output isolation separately, but preserve semantic stdout results
    // even when supplied configuration prevents safe report publication.
    let report_paths = args
        .report
        .as_ref()
        .map(|_| report_protected_paths(args.node_args.clone()));
    let mut report = validate_snapshot(&inputs, args.node_args, scratch.path())?;
    scratch.close()?;
    let console_result = (|| -> eyre::Result<()> {
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &report)?;
        writeln!(stdout)?;
        Ok(())
    })();
    if let Some(path) = args.report {
        if let Some(paths) = report_paths {
            report.protected_paths.0.extend(paths?.0);
        }
        write_report(&path, &report)?;
    }
    console_result?;
    eyre::ensure!(
        report.success(),
        "snapshot validation contains failed or incomplete checks"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::validation::report::{CheckName, CheckStatus};
    use clap::Parser;

    #[derive(Parser)]
    struct Arguments {
        #[command(flatten)]
        validate: ValidateArgs,
    }

    fn genesis(root: &Path) -> OsString {
        let output = root.join("genesis.json");
        crate::tee_genesis::run(&[
            "outbe-chain".into(),
            "tee".into(),
            "genesis".into(),
            "--input".into(),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../testing/e2e-harness/fixtures/ocomp-final-v1/artifacts/genesis-final.json"
            )
            .into(),
            "--output".into(),
            output.to_str().unwrap().into(),
            "--mode".into(),
            "gramine-direct-dev".into(),
        ])
        .unwrap();
        output.into_os_string()
    }

    fn provenance_args(report: Option<PathBuf>, node_args: Vec<OsString>) -> ValidateArgs {
        ValidateArgs {
            checks: "provenance".into(),
            manifest: None,
            signature: None,
            archive: None,
            expected_signer: None,
            report,
            node_args,
        }
    }

    #[test]
    fn report_publication_protects_native_roots_in_provenance_only_mode() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let config = root.join("projection.toml");
        std::fs::write(&config, "version = 1\nbackend = 'rocksdb'\n[rocksdb]\npath = 'projection'\nsecondary_path = 'secondary'\n").unwrap();
        let arguments: Vec<OsString> = vec![
            "--chain".into(),
            genesis(root),
            "--datadir".into(),
            root.join("chain").into_os_string(),
            "--consensus.storage-dir".into(),
            root.join("consensus").into_os_string(),
            "--consensus.keys-dir".into(),
            root.join("keys").into_os_string(),
            "--projection.storage-config".into(),
            config.into_os_string(),
            "--datadir.static-files".into(),
            root.join("static").into_os_string(),
            "--datadir.rocksdb".into(),
            root.join("execution-rocks").into_os_string(),
        ];
        for relative in [
            "chain/db",
            "static",
            "execution-rocks",
            "consensus",
            "ocomp/domain-v1",
            "keys",
            "projection",
        ] {
            let native = root.join(relative);
            std::fs::create_dir_all(&native).unwrap();
            let sentinel = native.join("sentinel");
            std::fs::write(&sentinel, b"native bytes").unwrap();
            let target = native.join("audit.json");
            assert!(run(provenance_args(Some(target.clone()), arguments.clone())).is_err());
            assert!(
                !target.exists(),
                "report changed native directory: {}",
                native.display()
            );
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"native bytes");
            assert_eq!(std::fs::read_dir(&native).unwrap().count(), 1);
        }
        let target = root.join("external-report.json");
        let outcome = run(provenance_args(Some(target.clone()), arguments));
        assert!(outcome.is_err());
        assert!(target.is_file(), "{outcome:?}");
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(target).unwrap()).unwrap();
        assert_eq!(report["checks"]["provenance"]["status"], "incomplete");
    }

    #[test]
    fn report_publication_refuses_unresolved_native_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("audit.json");
        let arguments = vec![
            "--datadir".into(),
            directory.path().into(),
            "--invalid-native-option".into(),
        ];
        assert!(run(provenance_args(Some(target.clone()), arguments)).is_err());
        assert!(
            !target.exists(),
            "unresolved roots must not permit report publication"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn report_publication_without_projection_configuration_is_supported() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("audit.json");
        let arguments = vec![
            "--chain".into(),
            genesis(directory.path()),
            "--datadir".into(),
            directory.path().join("chain").into_os_string(),
        ];
        let outcome = run(provenance_args(Some(target.clone()), arguments));
        assert!(outcome.is_err());
        assert!(target.is_file(), "{outcome:?}");
        assert!(!directory.path().join("chain").exists());
    }

    #[test]
    fn report_publication_for_artifacts_needs_no_native_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("audit.json");
        assert!(run(provenance_args(Some(target.clone()), Vec::new())).is_err());
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(target).unwrap()).unwrap();
        assert_eq!(report["checks"]["provenance"]["status"], "incomplete");
    }

    #[test]
    fn validation_options_preserve_ordinary_node_arguments() {
        let signer = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
        let args = Arguments::try_parse_from([
            "validate",
            "--checks",
            "evm,ocomp",
            "--manifest",
            "manifest.json",
            "--signature",
            "signature.json",
            "--archive",
            "snapshot.tar",
            "--expected-signer",
            signer,
            "--report",
            "audit.json",
            "--",
            "--datadir",
            "/data/new-node",
            "--chain",
            "genesis.json",
        ])
        .unwrap()
        .validate;
        assert_eq!(args.checks, "evm,ocomp");
        assert_eq!(args.manifest, Some("manifest.json".into()));
        assert_eq!(args.signature, Some("signature.json".into()));
        assert_eq!(args.archive, Some("snapshot.tar".into()));
        assert_eq!(hex::encode(args.expected_signer.unwrap()), signer);
        assert_eq!(args.report, Some("audit.json".into()));
        assert_eq!(
            args.node_args,
            ["--datadir", "/data/new-node", "--chain", "genesis.json"].map(OsString::from)
        );
        let defaults = Arguments::try_parse_from(["validate"]).unwrap().validate;
        assert_eq!(defaults.checks, "all");
        assert!(defaults.report.is_none());
        assert!(defaults.node_args.is_empty());
    }

    #[test]
    fn incomplete_report_is_written_but_is_not_a_success_or_startup_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("report.json");
        let mut report = ValidationReport::new([CheckName::Ocomp]);
        report.record(
            CheckName::Ocomp,
            CheckStatus::Incomplete,
            Some("missing request frame B=100"),
        );
        write_report(&target, &report).unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(json["checks"]["ocomp"]["status"], "incomplete");
        assert_eq!(json["checks"]["evm"]["status"], "not_requested");
        assert!(json.get("protected_paths").is_none());
        assert!(!report.success());
    }

    #[test]
    fn report_cannot_overwrite_existing_files_or_enter_known_native_roots() {
        let directory = tempfile::tempdir().unwrap();
        let native = directory.path().join("native");
        std::fs::create_dir(&native).unwrap();
        let existing = directory.path().join("operator-key");
        std::fs::write(&existing, b"operator-owned bytes").unwrap();
        let mut report = ValidationReport::new([CheckName::Headers]);
        report.protected_paths.0.push(native.clone());
        assert!(write_report(&existing, &report).is_err());
        assert_eq!(std::fs::read(existing).unwrap(), b"operator-owned bytes");
        assert!(write_report(&native.join("audit.json"), &report).is_err());
        assert_eq!(std::fs::read_dir(native).unwrap().count(), 0);
        assert!(
            write_report(&directory.path().join("absent-parent/report.json"), &report).is_err()
        );
        assert!(!directory.path().join("absent-parent").exists());
    }
}
