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
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        eyre::bail!("OCOMP bundle catalog must be a real directory");
    }
    for entry in std::fs::read_dir(&catalog_root).wrap_err("read OCOMP bundle catalog")? {
        let entry = entry.wrap_err("read OCOMP bundle catalog entry")?;
        let metadata = std::fs::symlink_metadata(entry.path())
            .wrap_err("inspect OCOMP bundle catalog entry")?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            eyre::bail!("OCOMP bundle catalog entries must be regular files");
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| eyre::eyre!("OCOMP bundle filename is not UTF-8"))?;
        let hash_hex = name
            .strip_suffix(".ocb1")
            .ok_or_else(|| eyre::eyre!("OCOMP bundle filename must end in .ocb1"))?;
        if hash_hex.len() != 64
            || !hash_hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            eyre::bail!("OCOMP bundle filename must be 64 lowercase hex characters plus .ocb1");
        }
        let canonical = std::fs::read(entry.path()).wrap_err("read installed OCOMP bundle")?;
        let bundle =
            outbe_ocomp::bundle::PinnedProtocolBundle::decode_canonical(&canonical, limits)
                .wrap_err("decode installed OCOMP bundle")?;
        if hex::encode(bundle.hash().as_slice()) != hash_hex {
            eyre::bail!("installed OCOMP bundle filename does not match its canonical hash");
        }
        if let Some(existing) = bundles.insert(bundle.hash(), bundle.clone()) {
            if existing != bundle {
                eyre::bail!("conflicting installed OCOMP bundle bytes");
            }
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
                eyre::bail!(
                    "configured OCOMP bundle hashes require a populated hash-addressed catalog"
                );
            }
        }
        return Ok(vec![initial]);
    }
    ordered_installed_ocomp_bundle_hashes(initial_hash, &bundles, configured_hashes)?
        .into_iter()
        .map(|hash| {
            bundles.get(&hash).cloned().ok_or_else(|| {
                eyre::eyre!("configured OCOMP bundle {hash} is not installed in the catalog")
            })
        })
        .collect()
}

pub(crate) fn ordered_installed_ocomp_bundle_hashes<V>(
    initial_hash: alloy_primitives::B256,
    bundles: &BTreeMap<alloy_primitives::B256, V>,
    configured_hashes: Option<&str>,
) -> eyre::Result<Vec<alloy_primitives::B256>> {
    if bundles.is_empty() || bundles.len() > 2 {
        eyre::bail!("OCOMP runtime supports exactly active plus one staged/retiring bundle");
    }
    if let Some(configured) = configured_hashes {
        let hashes = parse_ocomp_bundle_hashes(configured)?;
        if hashes.len() != bundles.len() || hashes.iter().any(|hash| !bundles.contains_key(hash)) {
            eyre::bail!("OCOMP bundle catalog must exactly match OCOMP_PROTOCOL_BUNDLE_HASHES");
        }
        return Ok(hashes);
    }

    if bundles.len() > 1 && !bundles.contains_key(&initial_hash) {
        eyre::bail!(
            "OCOMP_PROTOCOL_BUNDLE_HASHES is required to order a post-genesis two-bundle catalog"
        );
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
            .ok_or_else(|| eyre::eyre!("OCOMP bundle hash must have a 0x prefix"))?;
        if hex_value.len() != 64
            || !hex_value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            eyre::bail!("OCOMP bundle hash must be 64 lowercase hex characters after 0x");
        }
        let decoded = hex::decode(hex_value).wrap_err("decode configured OCOMP bundle hash")?;
        let hash = alloy_primitives::B256::from_slice(&decoded);
        if hashes.contains(&hash) {
            eyre::bail!("OCOMP bundle hash list contains a duplicate");
        }
        hashes.push(hash);
    }
    if hashes.is_empty() || hashes.len() > 2 {
        eyre::bail!("OCOMP bundle hash list must contain one or two adjacent authorities");
    }
    Ok(hashes)
}
