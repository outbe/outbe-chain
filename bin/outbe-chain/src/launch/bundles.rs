use crate::errors::{require, LaunchError};
use crate::*;

pub(crate) fn load_installed_ocomp_bundles(
    domain_root: &Path,
    initial: outbe_ocomp::bundle::PinnedProtocolBundle,
    configured_hashes: Option<&str>,
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> eyre::Result<Vec<outbe_ocomp::bundle::PinnedProtocolBundle>> {
    let catalog_root = domain_root.join("protocol-bundles-v1");
    let initial_hash = initial.hash();
    let mut bundles = BTreeMap::new();
    let metadata = match std::fs::symlink_metadata(&catalog_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return select_installed_ocomp_bundles(
                initial,
                initial_hash,
                bundles,
                configured_hashes,
            );
        }
        Err(error) => return Err(error).wrap_err("inspect OCOMP bundle catalog"),
    };
    require(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        LaunchError::BundleCatalogNotRealDirectory,
    )?;
    for entry in std::fs::read_dir(&catalog_root).wrap_err("read OCOMP bundle catalog")? {
        let entry = entry.wrap_err("read OCOMP bundle catalog entry")?;
        let metadata = std::fs::symlink_metadata(entry.path())
            .wrap_err("inspect OCOMP bundle catalog entry")?;
        require(
            metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
            LaunchError::BundleCatalogEntryNotRegularFile,
        )?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| LaunchError::BundleFilenameNotUtf8)?;
        let hash_hex = name
            .strip_suffix(".ocb1")
            .ok_or(LaunchError::BundleFilenameMissingExtension)?;
        require(
            hash_hex.len() == 64
                && hash_hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            LaunchError::BundleFilenameInvalidHex,
        )?;
        let canonical = std::fs::read(entry.path()).wrap_err("read installed OCOMP bundle")?;
        let bundle =
            outbe_ocomp::bundle::PinnedProtocolBundle::decode_canonical(&canonical, limits)
                .wrap_err("decode installed OCOMP bundle")?;
        require(
            hex::encode(bundle.hash().as_slice()) == hash_hex,
            LaunchError::BundleFilenameHashMismatch,
        )?;
        if let Some(existing) = bundles.insert(bundle.hash(), bundle.clone()) {
            require(existing == bundle, LaunchError::ConflictingBundleBytes)?;
        }
    }
    select_installed_ocomp_bundles(initial, initial_hash, bundles, configured_hashes)
}

fn select_installed_ocomp_bundles(
    initial: outbe_ocomp::bundle::PinnedProtocolBundle,
    initial_hash: alloy_primitives::B256,
    bundles: BTreeMap<alloy_primitives::B256, outbe_ocomp::bundle::PinnedProtocolBundle>,
    configured_hashes: Option<&str>,
) -> eyre::Result<Vec<outbe_ocomp::bundle::PinnedProtocolBundle>> {
    if bundles.is_empty() {
        if let Some(configured) = configured_hashes {
            let hashes = parse_ocomp_bundle_hashes(configured)?;
            if hashes.as_slice() != [initial_hash] {
                return Err(LaunchError::ConfiguredBundlesRequirePopulatedCatalog.into());
            }
        }
        return Ok(vec![initial]);
    }
    ordered_installed_ocomp_bundle_hashes(initial_hash, &bundles, configured_hashes)?
        .into_iter()
        .map(|hash| {
            bundles
                .get(&hash)
                .cloned()
                .ok_or_else(|| LaunchError::ConfiguredBundleNotInstalled { hash }.into())
        })
        .collect()
}

pub(crate) fn ordered_installed_ocomp_bundle_hashes<V>(
    initial_hash: alloy_primitives::B256,
    bundles: &BTreeMap<alloy_primitives::B256, V>,
    configured_hashes: Option<&str>,
) -> eyre::Result<Vec<alloy_primitives::B256>> {
    if bundles.is_empty() || bundles.len() > 2 {
        return Err(LaunchError::InvalidRuntimeBundleCount.into());
    }
    if let Some(configured) = configured_hashes {
        let hashes = parse_ocomp_bundle_hashes(configured)?;
        if hashes.len() != bundles.len() || hashes.iter().any(|hash| !bundles.contains_key(hash)) {
            return Err(LaunchError::BundleCatalogMismatchEnv.into());
        }
        return Ok(hashes);
    }

    if bundles.len() > 1 && !bundles.contains_key(&initial_hash) {
        return Err(LaunchError::TwoBundleCatalogRequiresEnvOrder.into());
    }
    let mut hashes = bundles.keys().copied().collect::<Vec<_>>();
    hashes.sort_by_key(|hash| *hash != initial_hash);
    Ok(hashes)
}

pub(crate) fn parse_ocomp_bundle_hashes(value: &str) -> eyre::Result<Vec<alloy_primitives::B256>> {
    let mut hashes = Vec::new();
    for encoded in value.split(',') {
        let hex_value = encoded
            .strip_prefix("0x")
            .ok_or(LaunchError::BundleHashMissingPrefix)?;
        if hex_value.len() != 64
            || !hex_value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(LaunchError::BundleHashInvalidHex.into());
        }
        let decoded = hex::decode(hex_value).wrap_err("decode configured OCOMP bundle hash")?;
        let hash = alloy_primitives::B256::from_slice(&decoded);
        if hashes.contains(&hash) {
            return Err(LaunchError::DuplicateBundleHash.into());
        }
        hashes.push(hash);
    }
    if hashes.is_empty() || hashes.len() > 2 {
        return Err(LaunchError::InvalidBundleHashListLength.into());
    }
    Ok(hashes)
}
