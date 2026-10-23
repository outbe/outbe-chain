use super::super::validate_archive_member_path;
use super::{file_digest, is_lower_hex, require_nonempty_regular_file};
use eyre::{bail, eyre, Result, WrapErr};
use outbe_tee::release_dcap_artifacts::ReleaseDcapArtifactSetV1;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::Path,
};

pub(in super::super) fn verify_processor_dcap_archive(
    archive_path: &Path,
    summary_path: &Path,
    evidence: &Value,
    artifact_set: ReleaseDcapArtifactSetV1,
    source_date_epoch: i64,
) -> Result<()> {
    if source_date_epoch < 0 {
        bail!("Processor DCAP archive SOURCE_DATE_EPOCH must be non-negative");
    }
    require_nonempty_regular_file(archive_path, "Processor DCAP evidence archive")?;
    let mut expected = expected_artifacts(evidence, artifact_set, summary_path)?;
    let input = File::open(archive_path)
        .wrap_err_with(|| format!("open Processor DCAP archive: {}", archive_path.display()))?;
    let mut archive = tar::Archive::new(input);
    let mut observed = BTreeSet::new();
    let mut previous = None::<String>;
    for item in archive
        .entries()
        .wrap_err("read Processor DCAP evidence archive")?
    {
        let mut item = item.wrap_err("read Processor DCAP evidence archive entry")?;
        let Some(path) = archive_member_path(&item)? else {
            continue;
        };
        if previous.as_ref().is_some_and(|value| value >= &path) {
            bail!("Processor DCAP archive members are not in canonical order");
        }
        previous = Some(path.clone());
        if !observed.insert(path.clone()) {
            bail!("Processor DCAP archive contains duplicate path: {path}");
        }
        verify_member(&mut item, &path, &mut expected, source_date_epoch)?;
    }
    if !expected.is_empty() {
        bail!(
            "Processor DCAP archive lacks retained files: {}",
            expected.keys().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(())
}

fn expected_artifacts(
    evidence: &Value,
    artifact_set: ReleaseDcapArtifactSetV1,
    summary_path: &Path,
) -> Result<BTreeMap<String, (u64, String)>> {
    let records = evidence
        .get("artifacts")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("Processor DCAP evidence lacks retained artifact records"))?;
    let required = artifact_set.paths();
    let declared = records.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if declared != required {
        bail!("Processor DCAP evidence does not declare the exact canonical artifact set");
    }
    let mut expected = BTreeMap::new();
    for (path, record) in records {
        validate_archive_member_path(path)?;
        let size = record
            .get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| eyre!("Processor DCAP artifact {path} lacks size"))?;
        let sha256 = record
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("Processor DCAP artifact {path} lacks sha256"))?;
        if !is_lower_hex(sha256, 64) {
            bail!("Processor DCAP artifact {path} has invalid sha256");
        }
        if expected
            .insert(path.clone(), (size, sha256.to_owned()))
            .is_some()
        {
            bail!("Processor DCAP evidence contains duplicate artifact {path}");
        }
    }
    let summary_size = fs::metadata(summary_path)?.len();
    let summary_digest = file_digest(summary_path)?;
    if expected
        .insert(
            "hardware-dcap-evidence.json".to_owned(),
            (summary_size, summary_digest.value),
        )
        .is_some()
    {
        bail!("Processor DCAP artifact records contain the reserved summary path");
    }
    Ok(expected)
}

fn archive_member_path<R: Read>(item: &tar::Entry<'_, R>) -> Result<Option<String>> {
    let is_root_entry = {
        let path = item.header().path_bytes();
        path.as_ref() == b"." || path.as_ref() == b"./"
    };
    if is_root_entry {
        if !item.header().entry_type().is_dir() {
            bail!("Processor DCAP archive root entry is not a directory");
        }
        return Ok(None);
    }
    let raw_path = item
        .path()
        .wrap_err("read Processor DCAP evidence archive path")?
        .to_string_lossy()
        .into_owned();
    let path = raw_path
        .strip_prefix("./")
        .unwrap_or(&raw_path)
        .trim_end_matches('/')
        .to_owned();
    validate_archive_member_path(&path)?;
    Ok(Some(path))
}

fn verify_member<R: Read>(
    item: &mut tar::Entry<'_, R>,
    path: &str,
    expected: &mut BTreeMap<String, (u64, String)>,
    source_date_epoch: i64,
) -> Result<()> {
    let header = item.header();
    if header.uid()? != 0 || header.gid()? != 0 || header.mtime()? != source_date_epoch as u64 {
        bail!("Processor DCAP archive has non-deterministic ownership/time: {path}");
    }
    if header.entry_type().is_dir() {
        let prefix = format!("{path}/");
        if !expected.keys().any(|member| member.starts_with(&prefix)) {
            bail!("Processor DCAP archive contains unrecorded directory: {path}");
        }
        return Ok(());
    }
    if !header.entry_type().is_file() {
        bail!("Processor DCAP archive contains non-file entry: {path}");
    }
    let (expected_size, expected_sha256) = expected
        .remove(path)
        .ok_or_else(|| eyre!("Processor DCAP archive contains unrecorded file: {path}"))?;
    if header.size()? != expected_size {
        bail!("Processor DCAP archive member size mismatch: {path}");
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let count = item.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        size += count as u64;
    }
    if size != expected_size || hex::encode(hasher.finalize()) != expected_sha256 {
        bail!("Processor DCAP archive member digest mismatch: {path}");
    }
    Ok(())
}
