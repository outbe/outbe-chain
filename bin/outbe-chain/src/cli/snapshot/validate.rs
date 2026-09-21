use crate::snapshot::validation::{
    report::ValidationReport,
    run::{validate_snapshot, ValidationInputs},
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
    let report = validate_snapshot(&inputs, args.node_args, scratch.path())?;
    scratch.close()?;
    let console_result = (|| -> eyre::Result<()> {
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &report)?;
        writeln!(stdout)?;
        Ok(())
    })();
    if let Some(path) = args.report {
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
