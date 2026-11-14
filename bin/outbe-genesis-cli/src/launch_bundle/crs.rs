//! Fetch and verify the fixed BN254 G1 SRS shipped with validator bundles.

use std::fs;
use std::path::{Path, PathBuf};

use reqwest::header::RANGE;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::GenesisError;

const SPEC_JSON: &str = include_str!("../../../../release/bn254-g1-srs-v1.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CrsSpec {
    version: u32,
    file_name: String,
    url: String,
    size: u64,
    sha256: String,
}

/// Ensure the pinned CRS is available below the generated launch-bundle root.
pub fn stage(output_dir: &Path) -> Result<PathBuf, GenesisError> {
    let spec = spec()?;
    let target = output_dir.join("crs").join(&spec.file_name);
    if target.is_file() {
        verify_file(&target, &spec)?;
        return Ok(target);
    }

    if let Some(cached) = cached_crs(&spec) {
        verify_file(&cached, &spec)?;
        copy(&cached, &target)?;
        return Ok(target);
    }

    let end = spec
        .size
        .checked_sub(1)
        .ok_or_else(|| GenesisError::validation("BN254 CRS size must be positive"))?;
    let response = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|error| GenesisError::validation(format!("build CRS client: {error}")))?
        .get(&spec.url)
        .header(RANGE, format!("bytes=0-{end}"))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| GenesisError::validation(format!("download BN254 CRS: {error}")))?;
    let bytes = response
        .bytes()
        .map_err(|error| GenesisError::validation(format!("read BN254 CRS: {error}")))?;
    verify_bytes(&bytes, &spec)?;
    write(&target, &bytes)?;
    Ok(target)
}

fn spec() -> Result<CrsSpec, GenesisError> {
    let spec: CrsSpec = serde_json::from_str(SPEC_JSON)?;
    if spec.version != 1
        || spec.file_name != "bn254_g1.dat"
        || !spec.url.starts_with("https://")
        || spec.sha256.len() != 64
        || !spec.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(GenesisError::validation("invalid bundled BN254 CRS spec"));
    }
    Ok(spec)
}

fn cached_crs(spec: &CrsSpec) -> Option<PathBuf> {
    std::env::var_os("BB_CRS_PATH")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".bb-crs").join(&spec.file_name))
                .filter(|path| path.is_file())
        })
}

fn verify_file(path: &Path, spec: &CrsSpec) -> Result<(), GenesisError> {
    let bytes = fs::read(path).map_err(|error| GenesisError::io(path, error))?;
    verify_bytes(&bytes, spec)
}

fn verify_bytes(bytes: &[u8], spec: &CrsSpec) -> Result<(), GenesisError> {
    if bytes.len() as u64 != spec.size {
        return Err(GenesisError::validation(format!(
            "BN254 CRS has size {}, expected {}",
            bytes.len(),
            spec.size
        )));
    }
    let digest = hex::encode(Sha256::digest(bytes));
    if digest != spec.sha256 {
        return Err(GenesisError::validation(format!(
            "BN254 CRS checksum mismatch: got {digest}, expected {}",
            spec.sha256
        )));
    }
    Ok(())
}

fn copy(source: &Path, target: &Path) -> Result<(), GenesisError> {
    let bytes = fs::read(source).map_err(|error| GenesisError::io(source, error))?;
    write(target, &bytes)
}

fn write(target: &Path, bytes: &[u8]) -> Result<(), GenesisError> {
    let parent = target
        .parent()
        .ok_or_else(|| GenesisError::validation("BN254 CRS target has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| GenesisError::io(parent, error))?;
    let temporary = target.with_extension("dat.tmp");
    fs::write(&temporary, bytes).map_err(|error| GenesisError::io(&temporary, error))?;
    fs::rename(&temporary, target).map_err(|error| GenesisError::io(target, error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_spec_matches_canonical_capacity() {
        let spec = spec().expect("valid spec");
        assert_eq!(spec.size, u64::from(((1_u32 << 16) + 1) * 64));
        assert_eq!(
            spec.sha256,
            "9fd9fad54cfdc61ec309e818fe7bbddad1ca7dde72cc9b1afd126b744f887078"
        );
    }
}
