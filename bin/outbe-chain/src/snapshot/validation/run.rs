//! Resolve operator-selected checks before opening any native store.

use std::collections::BTreeSet;

use super::report::CheckName;

pub(crate) struct CheckSelection {
    pub checks: BTreeSet<CheckName>,
}

impl CheckSelection {
    pub(crate) fn resolve(
        requested: &str,
        has_artifact_metadata: bool,
        has_expected_signer: bool,
    ) -> eyre::Result<Self> {
        use CheckName::*;
        let mut checks = BTreeSet::new();
        if requested == "all" {
            checks.extend([Headers, Evm, Ce, Bodies, Ocomp]);
            if has_artifact_metadata {
                checks.extend([Files, Provenance]);
            }
        } else {
            for name in requested.split(',') {
                checks.insert(match name {
                    "files" => Files,
                    "provenance" => Provenance,
                    "headers" => Headers,
                    "evm" => Evm,
                    "ce" => Ce,
                    "bodies" => Bodies,
                    "ocomp" => Ocomp,
                    _ => eyre::bail!("unknown snapshot check {name:?}"),
                });
            }
        }
        if has_expected_signer || checks.contains(&Files) {
            checks.insert(Provenance);
        }
        if checks.contains(&Bodies) {
            checks.insert(Ce);
        }
        if checks.contains(&Ocomp) {
            checks.insert(Evm);
        }
        if checks.contains(&Evm) || checks.contains(&Ce) {
            checks.insert(Headers);
        }
        Ok(Self { checks })
    }

    pub(crate) fn needs_projection(&self) -> bool {
        [CheckName::Files, CheckName::Bodies, CheckName::Ocomp]
            .iter()
            .any(|check| self.checks.contains(check))
    }
}

/// Optional signed artifact inputs. Native check selection remains independent.
pub(crate) struct ValidationInputs {
    pub checks: String,
    pub manifest: Option<std::path::PathBuf>,
    pub signature: Option<std::path::PathBuf>,
    pub archive: Option<std::path::PathBuf>,
    pub expected_signer: Option<[u8; 33]>,
}

impl Default for ValidationInputs {
    fn default() -> Self {
        Self {
            checks: "all".into(),
            manifest: None,
            signature: None,
            archive: None,
            expected_signer: None,
        }
    }
}

/// Component evidence only: an archive result does not verify installed native files.
pub(crate) struct ArtifactAudit {
    pub metadata: eyre::Result<outbe_snapshot::manifest::SnapshotManifestV1>,
    pub provenance: super::report::ProvenanceObservation,
    pub provenance_result: eyre::Result<()>,
    pub archive_result: Option<eyre::Result<()>>,
}

const MANIFEST_LIMIT: u64 = 256 * 1024 * 1024;
const SIGNATURE_LIMIT: u64 = 64 * 1024;

/// Keep the native anchored descriptor and path identity throughout both passes.
struct ArtifactSource {
    root: outbe_snapshot::fs::SourceRoot,
    member: std::path::PathBuf,
    entry: outbe_snapshot::fs::SourceEntry,
}

impl ArtifactSource {
    fn open(path: &std::path::Path) -> eyre::Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let member = path
            .file_name()
            .ok_or_else(|| eyre::eyre!("artifact path has no filename"))?;
        let root = outbe_snapshot::fs::SourceRoot::open(parent).map_err(artifact_io)?;
        let entry = root
            .open_entry(std::path::Path::new(member))
            .map_err(artifact_io)?;
        eyre::ensure!(
            !entry.identity.is_directory,
            "artifact is not a regular file"
        );
        Ok(Self {
            root,
            member: member.into(),
            entry,
        })
    }

    fn verify_unchanged(&self) -> eyre::Result<()> {
        self.entry.verify_unchanged()?;
        self.root.verify_unchanged()?;
        self.root.reopen(&self.member, &self.entry.identity)?;
        Ok(())
    }
}

fn artifact_io(error: std::io::Error) -> eyre::Report {
    if error.kind() == std::io::ErrorKind::NotFound {
        eyre::Report::new(error).wrap_err(super::Incomplete(
            "required artifact metadata is missing".into(),
        ))
    } else {
        error.into()
    }
}

/// Reports own their diagnostics; preserve the typed unavailable-input distinction.
fn artifact_error(error: &eyre::Report) -> eyre::Report {
    if error.downcast_ref::<super::Incomplete>().is_some() {
        super::Incomplete(format!("{error:#}")).into()
    } else {
        eyre::eyre!("{error:#}")
    }
}

fn missing_artifact(name: &str) -> eyre::Report {
    super::Incomplete(format!("required artifact {name} is missing")).into()
}

fn read_sidecar(path: &std::path::Path, limit: u64) -> eyre::Result<Vec<u8>> {
    use std::io::Read;
    let mut source = ArtifactSource::open(path)?;
    eyre::ensure!(
        source.entry.identity.size <= limit,
        "artifact metadata exceeds size limit"
    );
    let mut bytes = Vec::new();
    (&mut source.entry.file)
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    eyre::ensure!(
        bytes.len() as u64 <= limit,
        "artifact metadata exceeds size limit"
    );
    source.verify_unchanged()?;
    Ok(bytes)
}

fn archive_prefix(source: &mut ArtifactSource) -> (eyre::Result<Vec<u8>>, eyre::Result<Vec<u8>>) {
    use std::io::Read;
    // Bound even tar extension processing before the two accepted metadata members.
    // No payload entry is requested during this pass.
    let reader = (&mut source.entry.file).take(MANIFEST_LIMIT + SIGNATURE_LIMIT + 4096);
    let mut archive = tar::Archive::new(reader);
    let mut entries = match archive.entries() {
        Ok(entries) => entries,
        Err(error) => {
            let error = eyre::Report::new(error);
            return (Err(artifact_error(&error)), Err(error));
        }
    };
    let mut metadata = |name: &str, limit| -> eyre::Result<Vec<u8>> {
        let mut entry = entries.next().ok_or_else(|| missing_artifact(name))??;
        eyre::ensure!(
            entry.path()?.as_ref() == std::path::Path::new(name)
                && entry.header().entry_type().is_file()
                && entry.size() <= limit,
            "invalid {name} archive member"
        );
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let raw = metadata("manifest.json", MANIFEST_LIMIT);
    let signature = match &raw {
        Ok(_) => metadata("signature.json", SIGNATURE_LIMIT),
        Err(error) => Err(artifact_error(error)),
    };
    (raw, signature)
}

pub(crate) fn audit_artifact(inputs: &ValidationInputs, files_requested: bool) -> ArtifactAudit {
    use outbe_snapshot::{manifest::SnapshotManifestV1, provenance::SignatureEnvelope};
    use std::io::{Seek, SeekFrom};

    let mut archive = inputs
        .archive
        .as_deref()
        .map(ArtifactSource::open)
        .transpose();
    let mut side_manifest = inputs
        .manifest
        .as_deref()
        .map(|path| read_sidecar(path, MANIFEST_LIMIT));
    let mut side_signature = inputs
        .signature
        .as_deref()
        .map(|path| read_sidecar(path, SIGNATURE_LIMIT));
    let (raw, signature) = match &mut archive {
        Ok(Some(source)) => archive_prefix(source),
        Err(error) => (Err(artifact_error(error)), Err(artifact_error(error))),
        Ok(None) => (
            side_manifest
                .take()
                .unwrap_or_else(|| Err(missing_artifact("manifest"))),
            side_signature
                .take()
                .unwrap_or_else(|| Err(missing_artifact("signature"))),
        ),
    };
    let agreement = (|| -> eyre::Result<()> {
        if inputs.archive.is_some() {
            if let Some(supplied) = &side_manifest {
                let supplied = supplied.as_ref().map_err(artifact_error)?;
                let raw = raw.as_ref().map_err(artifact_error)?;
                eyre::ensure!(
                    supplied == raw,
                    "detached manifest differs from exact archive manifest bytes"
                );
            }
        }
        Ok(())
    })();
    let mut metadata = (|| -> eyre::Result<SnapshotManifestV1> {
        agreement.as_ref().map_err(artifact_error)?;
        Ok(SnapshotManifestV1::from_bytes(
            raw.as_ref().map_err(artifact_error)?,
        )?)
    })();
    let mut provenance = super::report::ProvenanceObservation::default();
    let mut provenance_result = (|| -> eyre::Result<()> {
        let raw = raw.as_ref().map_err(artifact_error)?;
        let signature = signature.as_ref().map_err(artifact_error)?;
        provenance.signature_valid = Some(false);
        let signer = SignatureEnvelope::from_bytes(signature)?.verify(raw, None)?;
        provenance.signature_valid = Some(true);
        provenance.signer = Some(hex::encode(signer));
        agreement.as_ref().map_err(artifact_error)?;
        if inputs.archive.is_some() {
            if let Some(supplied) = &side_signature {
                let supplied = supplied.as_ref().map_err(artifact_error)?;
                let other = SignatureEnvelope::from_bytes(supplied)?.verify(raw, None)?;
                eyre::ensure!(
                    other == signer,
                    "detached signature has a different authenticated creator"
                );
            }
        }
        if let Some(expected) = inputs.expected_signer {
            provenance.expected_signer_match = Some(expected == signer);
            eyre::ensure!(
                expected == signer,
                "snapshot creator does not match expected public key"
            );
        }
        Ok(())
    })();
    let mut archive_result = if files_requested && inputs.archive.is_some() {
        Some((|| -> eyre::Result<()> {
            let source = archive
                .as_mut()
                .map_err(|e| artifact_error(e))?
                .as_mut()
                .ok_or_else(|| missing_artifact("archive"))?;
            let raw = raw.as_ref().map_err(artifact_error)?;
            // Preserve unavailable metadata as Incomplete; the full archive
            // reader otherwise reports a missing signature as InvalidData.
            signature.as_ref().map_err(artifact_error)?;
            source.entry.file.seek(SeekFrom::Start(0))?;
            let index = outbe_snapshot::archive::read_archive_index(&mut source.entry.file, None)?;
            eyre::ensure!(
                &index.raw_manifest == raw,
                "archive manifest changed between metadata and payload passes"
            );
            Ok(())
        })())
    } else {
        None
    };
    if let Ok(Some(source)) = &archive {
        if let Err(error) = source.verify_unchanged() {
            metadata = Err(artifact_error(&error));
            provenance_result = Err(artifact_error(&error));
            if let Some(result) = &mut archive_result {
                *result = Err(error);
            }
        }
    }
    ArtifactAudit {
        metadata,
        provenance,
        provenance_result,
        archive_result,
    }
}
