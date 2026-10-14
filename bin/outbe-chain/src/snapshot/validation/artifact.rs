//! Signed artifact metadata, provenance, and installed native file inventory checks.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use k256::sha2::{Digest, Sha256};
use outbe_snapshot::{
    fs::{FileIdentity, SourceEntry, SourceRoot},
    manifest::{DomainInventory, EntryKind, FileEntry, SnapshotManifestV1},
    provenance::SignatureEnvelope,
};

use super::{
    super::{
        config::NativeLayout,
        inventory::{enumerate_native_files, NativeDomain, NativeInventory},
    },
    report::ProvenanceObservation,
    run::ValidationInputs,
};
use crate::errors::{require, SnapshotError};

/// Component evidence only: an archive result does not verify installed native files.
pub(crate) struct ArtifactAudit {
    pub metadata: eyre::Result<SnapshotManifestV1>,
    pub provenance: ProvenanceObservation,
    pub provenance_result: eyre::Result<()>,
    pub archive_result: Option<eyre::Result<()>>,
}

const MANIFEST_LIMIT: u64 = 256 * 1024 * 1024;
const SIGNATURE_LIMIT: u64 = 64 * 1024;

/// Keep the native anchored descriptor and path identity throughout both passes.
struct ArtifactSource {
    root: SourceRoot,
    member: PathBuf,
    entry: SourceEntry,
}

impl ArtifactSource {
    fn open(path: &Path) -> eyre::Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let member = path
            .file_name()
            .ok_or(SnapshotError::ArtifactPathHasNoFilename)?;
        let root = SourceRoot::open(parent).map_err(artifact_io)?;
        let entry = root.open_entry(Path::new(member)).map_err(artifact_io)?;
        require(
            !entry.identity.is_directory,
            SnapshotError::ArtifactNotRegularFile,
        )?;
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

/// Reports own their diagnostics. Preserve the typed unavailable-input distinction.
pub(super) fn artifact_error(error: &eyre::Report) -> eyre::Report {
    if error.downcast_ref::<super::Incomplete>().is_some() {
        super::Incomplete(format!("{error:#}")).into()
    } else {
        SnapshotError::ErrorReport {
            message: format!("{error:#}"),
        }
        .into()
    }
}

pub(super) fn missing_artifact(name: &str) -> eyre::Report {
    super::Incomplete(format!("required artifact {name} is missing")).into()
}

fn read_sidecar(path: &Path, limit: u64) -> eyre::Result<Vec<u8>> {
    let mut source = ArtifactSource::open(path)?;
    require(
        source.entry.identity.size <= limit,
        SnapshotError::ArtifactMetadataExceedsLimit,
    )?;
    let mut bytes = Vec::new();
    (&mut source.entry.file)
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    require(
        bytes.len() as u64 <= limit,
        SnapshotError::ArtifactMetadataExceedsLimit,
    )?;
    source.verify_unchanged()?;
    Ok(bytes)
}

fn archive_prefix(source: &mut ArtifactSource) -> (eyre::Result<Vec<u8>>, eyre::Result<Vec<u8>>) {
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
        require(
            entry.path()?.as_ref() == Path::new(name)
                && entry.header().entry_type().is_file()
                && entry.size() <= limit,
            SnapshotError::InvalidArchiveMember {
                name: name.to_owned(),
            },
        )?;
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

fn check_detached_manifest_agreement(
    has_archive: bool,
    side_manifest: Option<&eyre::Result<Vec<u8>>>,
    raw: &eyre::Result<Vec<u8>>,
) -> eyre::Result<()> {
    if !has_archive {
        return Ok(());
    }
    let Some(supplied) = side_manifest else {
        return Ok(());
    };
    let supplied = supplied.as_ref().map_err(artifact_error)?;
    let raw = raw.as_ref().map_err(artifact_error)?;
    require(supplied == raw, SnapshotError::DetachedManifestMismatch)?;
    Ok(())
}

fn verify_artifact_provenance(
    inputs: &ValidationInputs,
    (raw, signature): (&eyre::Result<Vec<u8>>, &eyre::Result<Vec<u8>>),
    (agreement, side_signature): (&eyre::Result<()>, Option<&eyre::Result<Vec<u8>>>),
    provenance: &mut ProvenanceObservation,
) -> eyre::Result<()> {
    let raw = raw.as_ref().map_err(artifact_error)?;
    let signature = signature.as_ref().map_err(artifact_error)?;
    provenance.signature_valid = Some(false);
    let signer = SignatureEnvelope::from_bytes(signature)?.verify(raw, None)?;
    provenance.signature_valid = Some(true);
    provenance.signer = Some(hex::encode(signer));
    agreement.as_ref().map_err(artifact_error)?;
    if inputs.archive.is_some() {
        if let Some(supplied) = side_signature {
            let supplied = supplied.as_ref().map_err(artifact_error)?;
            let other = SignatureEnvelope::from_bytes(supplied)?.verify(raw, None)?;
            require(
                other == signer,
                SnapshotError::DetachedSignatureCreatorMismatch,
            )?;
        }
    }
    if let Some(expected) = inputs.expected_signer {
        provenance.expected_signer_match = Some(expected == signer);
        require(
            expected == signer,
            SnapshotError::SnapshotCreatorKeyMismatch,
        )?;
    }
    Ok(())
}

fn verify_archive_payload_pass(
    archive: &mut eyre::Result<Option<ArtifactSource>>,
    raw: &eyre::Result<Vec<u8>>,
    signature: &eyre::Result<Vec<u8>>,
) -> eyre::Result<()> {
    let source = archive
        .as_mut()
        .map_err(|e| artifact_error(e))?
        .as_mut()
        .ok_or_else(|| missing_artifact("archive"))?;
    let raw = raw.as_ref().map_err(artifact_error)?;
    // Preserve unavailable metadata as Incomplete. The full archive
    // reader otherwise reports a missing signature as InvalidData.
    signature.as_ref().map_err(artifact_error)?;
    source.entry.file.seek(SeekFrom::Start(0))?;
    let index = outbe_snapshot::archive::read_archive_index(&mut source.entry.file, None)?;
    require(
        &index.raw_manifest == raw,
        SnapshotError::ArchiveManifestChangedBetweenPasses,
    )?;
    Ok(())
}

pub(crate) fn audit_artifact(inputs: &ValidationInputs, files_requested: bool) -> ArtifactAudit {
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
    let agreement =
        check_detached_manifest_agreement(inputs.archive.is_some(), side_manifest.as_ref(), &raw);
    let mut metadata = (|| -> eyre::Result<SnapshotManifestV1> {
        agreement.as_ref().map_err(artifact_error)?;
        Ok(SnapshotManifestV1::from_bytes(
            raw.as_ref().map_err(artifact_error)?,
        )?)
    })();
    let mut provenance = ProvenanceObservation::default();
    let mut provenance_result = verify_artifact_provenance(
        inputs,
        (&raw, &signature),
        (&agreement, side_signature.as_ref()),
        &mut provenance,
    );
    let mut archive_result = (files_requested && inputs.archive.is_some())
        .then(|| verify_archive_payload_pass(&mut archive, &raw, &signature));
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

fn match_declared_domains<'a>(
    inventory: &NativeInventory,
    manifest: &'a SnapshotManifestV1,
) -> eyre::Result<Vec<Option<&'a DomainInventory>>> {
    let mut declared = vec![None; inventory.domains.len()];
    for domain in &manifest.domains {
        let index = inventory
            .domains
            .iter()
            .position(|native| {
                native.kind == domain.kind && native.native_root == domain.native_root
            })
            .ok_or(SnapshotError::ManifestDomainNoMatchingPopulation { kind: domain.kind })?;
        require(
            declared[index].is_none(),
            SnapshotError::DuplicatePopulationInManifest { kind: domain.kind },
        )?;
        declared[index] = Some(domain);
    }
    Ok(declared)
}

fn verify_regular_file_member(
    entry: &mut SourceEntry,
    expected: &FileEntry,
    name: &str,
    (files, bytes): &mut (u64, u64),
) -> eyre::Result<()> {
    require(
        entry.identity.size == expected.size,
        SnapshotError::MemberSizeDiffers {
            name: name.to_owned(),
        },
    )?;
    let limit = expected
        .size
        .checked_add(1)
        .ok_or(SnapshotError::FileSizeOverflow)?;
    let mut reader = (&mut entry.file).take(limit);
    let mut hash = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length = length
            .checked_add(count as u64)
            .ok_or(SnapshotError::FileLengthOverflow)?;
        hash.update(&buffer[..count]);
    }
    require(
        length == expected.size,
        SnapshotError::MemberLengthChanged {
            name: name.to_owned(),
        },
    )?;
    require(
        expected.sha256.as_deref() == Some(hex::encode(hash.finalize()).as_str()),
        SnapshotError::MemberChecksumDiffers {
            name: name.to_owned(),
        },
    )?;
    *files = files
        .checked_add(1)
        .ok_or(SnapshotError::FileCountOverflow)?;
    *bytes = bytes
        .checked_add(length)
        .ok_or(SnapshotError::ByteCountOverflow)?;
    Ok(())
}

fn verify_domain_members<'a>(
    native: &'a NativeDomain,
    domain: &DomainInventory,
    totals: &mut (u64, u64),
) -> eyre::Result<Option<(SourceRoot, Vec<(&'a PathBuf, FileIdentity)>)>> {
    let wanted: BTreeMap<_, _> = domain
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let names = native
        .members
        .iter()
        .map(|path| {
            path.to_str().ok_or_else(|| {
                SnapshotError::MemberPathNotUtf8 {
                    path: path.display().to_string(),
                }
                .into()
            })
        })
        .collect::<eyre::Result<BTreeSet<_>>>()?;
    require(
        names == wanted.keys().copied().collect(),
        SnapshotError::MemberInventoryDiffers { kind: native.kind },
    )?;
    // The writer does not observe roots or modes of empty optional populations.
    if native.members.is_empty() {
        return Ok(None);
    }
    let root = SourceRoot::open(&native.root)?;
    require(
        root.open_entry(Path::new(""))?.identity.mode & 0o7777 == domain.mode,
        SnapshotError::RootModeDiffers { kind: native.kind },
    )?;
    let mut observed = Vec::new();
    for member in &native.members {
        let name = member
            .to_str()
            .ok_or(SnapshotError::MemberPathNotUtf8Plain)?;
        let expected = wanted.get(name).ok_or(SnapshotError::MemberNotDeclared)?;
        let mut entry = root.open_entry(member)?;
        require(
            entry.identity.mode & 0o7777 == expected.mode,
            SnapshotError::MemberModeDiffers {
                name: name.to_owned(),
            },
        )?;
        require(
            entry.identity.is_directory == (expected.kind == EntryKind::Directory),
            SnapshotError::MemberKindDiffers {
                name: name.to_owned(),
            },
        )?;
        if !entry.identity.is_directory {
            verify_regular_file_member(&mut entry, expected, name, totals)?;
        }
        entry.verify_unchanged()?;
        observed.push((member, entry.identity));
    }
    Ok(Some((root, observed)))
}

fn verify_inventory_unchanged(
    layout: &NativeLayout,
    inventory: &NativeInventory,
    opened: Vec<(SourceRoot, Vec<(&PathBuf, FileIdentity)>)>,
) -> eyre::Result<()> {
    let after = enumerate_native_files(layout)?;
    require(
        after.domains.len() == inventory.domains.len(),
        SnapshotError::DomainsChangedDuringVerification,
    )?;
    for before in &inventory.domains {
        let current = after
            .domains
            .iter()
            .find(|domain| domain.kind == before.kind && domain.native_root == before.native_root)
            .ok_or(SnapshotError::DomainDisappearedDuringVerification)?;
        require(
            current.root == before.root
                && current.members.iter().collect::<BTreeSet<_>>()
                    == before.members.iter().collect::<BTreeSet<_>>(),
            SnapshotError::MemberInventoryChangedDuringVerification,
        )?;
    }
    for (root, observed) in opened {
        root.verify_unchanged()?;
        for (member, identity) in observed {
            root.reopen(member, &identity)?;
        }
    }
    Ok(())
}

/// Compare installed native files to a structurally valid inventory. All host
/// roots and member opens come from native enumeration, never donor locations.
/// This establishes file equality only, independently of native semantic checks.
pub(crate) fn verify_native_files(
    layout: &NativeLayout,
    manifest: &SnapshotManifestV1,
) -> eyre::Result<()> {
    manifest.validate()?;
    // The owner enforces required native populations even when omitted by a donor.
    let inventory = enumerate_native_files(layout)?;
    let declared = match_declared_domains(&inventory, manifest)?;
    let mut opened = Vec::new();
    let mut totals = (0_u64, 0_u64);
    for (native, declared) in inventory.domains.iter().zip(declared) {
        let Some(domain) = declared else {
            require(
                native.members.is_empty(),
                SnapshotError::ManifestOmitsNonemptyPopulation { kind: native.kind },
            )?;
            continue;
        };
        if let Some(domain_opened) = verify_domain_members(native, domain, &mut totals)? {
            opened.push(domain_opened);
        }
    }
    require(
        totals == (manifest.file_count, manifest.total_bytes),
        SnapshotError::FileInventoryTotalsDiffer,
    )?;
    verify_inventory_unchanged(layout, &inventory, opened)
}
