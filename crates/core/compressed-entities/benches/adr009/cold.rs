//! Operator-driven cold-cache preparation and measurement.

use super::*;

fn sidecar_path(directory: &Path) -> PathBuf {
    let mut path = directory.as_os_str().to_os_string();
    path.push(".adr009.json");
    PathBuf::from(path)
}

pub(super) fn cold_prepare(arguments: &[String]) -> Result<(), String> {
    if arguments.len() != 4 {
        return Err(
            "usage: cold-prepare <K> <small|large> <uniform|concentrated> <insert|update|delete|mixed>"
                .to_owned(),
        );
    }
    let shard_count = arguments[0]
        .parse::<u32>()
        .map_err(|error| format!("invalid K: {error}"))?;
    if !K_CANDIDATES.contains(&shard_count) {
        return Err(format!("K must be one of {K_CANDIDATES:?}"));
    }
    let shape = shape(&arguments[1]).ok_or_else(|| "invalid shape".to_owned())?;
    let distribution =
        Distribution::parse(&arguments[2]).ok_or_else(|| "invalid distribution".to_owned())?;
    let operation =
        Operation::parse(&arguments[3]).ok_or_else(|| "invalid operation".to_owned())?;
    let Fixture {
        directory,
        service,
        parent,
        block_hash,
        mutations: _,
        dataset_checksum,
        descriptor,
        mdbx_bytes_before,
        mdbx_allocated_bytes_before,
    } = build_fixture(shard_count, shape, distribution, operation);
    drop(service);
    let directory = directory.keep();
    let sidecar = sidecar_path(&directory);
    let record = serde_json::json!({
        "version": 1,
        "fixture_dir": directory.display().to_string(),
        "case": descriptor,
        "shard_count": shard_count,
        "shape": shape.label,
        "distribution": distribution.label(),
        "operation": operation.label(),
        "parent_block_number": parent.block_number,
        "parent_block_hash": format!("{:#x}", parent.block_hash),
        "parent_root": format!("{:#x}", parent.root),
        "block_hash": format!("{:#x}", block_hash),
        "dataset_checksum": format!("{:#x}", dataset_checksum),
        "mdbx_bytes_before": mdbx_bytes_before,
        "mdbx_allocated_bytes_before": mdbx_allocated_bytes_before,
    });
    fs::write(
        &sidecar,
        serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("write cold sidecar: {error}"))?;
    println!(
        "ADR009_COLD_FIXTURE {}",
        serde_json::json!({
            "fixture_dir": directory.display().to_string(),
            "sidecar": sidecar.display().to_string(),
        })
    );
    Ok(())
}

pub(super) fn cold_run(arguments: &[String]) -> Result<(), String> {
    if arguments.len() != 2 || arguments[1] != "cache-drop-reviewed-and-completed" {
        return Err("usage: cold-run <fixture-dir> cache-drop-reviewed-and-completed".to_owned());
    }
    let directory = PathBuf::from(&arguments[0]);
    let metadata: serde_json::Value = serde_json::from_slice(
        &fs::read(sidecar_path(&directory))
            .map_err(|error| format!("read cold sidecar: {error}"))?,
    )
    .map_err(|error| format!("decode cold sidecar: {error}"))?;
    let text = |name: &str| -> Result<&str, String> {
        metadata[name]
            .as_str()
            .ok_or_else(|| format!("missing string field {name}"))
    };
    let number = |name: &str| -> Result<u64, String> {
        metadata[name]
            .as_u64()
            .ok_or_else(|| format!("missing integer field {name}"))
    };
    if number("version")? != 1 {
        return Err("unsupported cold fixture version".to_owned());
    }
    let shard_count = u32::try_from(number("shard_count")?)
        .map_err(|_| "shard_count does not fit u32".to_owned())?;
    let shape = shape(text("shape")?).ok_or_else(|| "invalid shape".to_owned())?;
    let distribution = Distribution::parse(text("distribution")?)
        .ok_or_else(|| "invalid distribution".to_owned())?;
    let operation =
        Operation::parse(text("operation")?).ok_or_else(|| "invalid operation".to_owned())?;
    let parent = ExactParentIdentity {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        block_number: number("parent_block_number")?,
        block_hash: text("parent_block_hash")?
            .parse()
            .map_err(|error| format!("invalid parent block hash: {error}"))?,
        root: text("parent_root")?
            .parse()
            .map_err(|error| format!("invalid parent root: {error}"))?,
    };
    let block_hash: B256 = text("block_hash")?
        .parse()
        .map_err(|error| format!("invalid block hash: {error}"))?;
    let expected_dataset_checksum: B256 = text("dataset_checksum")?
        .parse()
        .map_err(|error| format!("invalid dataset checksum: {error}"))?;
    let mdbx_bytes_before = number("mdbx_bytes_before")?;
    let mdbx_allocated_bytes_before = number("mdbx_allocated_bytes_before")?;
    let workload = workload(shape, distribution, operation);
    if workload.dataset_checksum != expected_dataset_checksum {
        return Err("cold fixture dataset checksum drift".to_owned());
    }

    let genesis_hash = B256::repeat_byte(9);
    let genesis_root =
        outbe_compressed_entities::sealed_root(B256::ZERO).map_err(|error| error.to_string())?;
    let genesis = FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: genesis_root,
    };
    let started = Instant::now();
    let db = CeMdbx::open(&directory, environment(shard_count, genesis_hash), genesis)
        .map_err(|error| format!("open cold MDBX fixture: {error}"))?;
    let service = CompressedTreeService::new(
        db,
        CandidateCacheLimits {
            max_candidates: 2,
            max_encoded_bytes: 256 * 1024 * 1024,
        },
    )
    .map_err(|error| format!("open cold tree service: {error}"))?;
    let parent_tree = service
        .open_parent(parent)
        .map_err(|error| format!("open cold exact parent: {error}"))?;
    let provisional = parent_tree
        .prepare_seal(2, &workload.mutations, &[])
        .map_err(|error| format!("cold proof and seal: {error}"))?;
    let root = provisional.new_root();
    service
        .publish_candidate(block_hash, provisional)
        .map_err(|error| format!("cold candidate publication: {error}"))?;
    let candidate = service
        .candidate(2, block_hash)
        .map_err(|error| format!("cold candidate lookup: {error}"))?
        .ok_or_else(|| "cold candidate missing after publication".to_owned())?;
    service
        .apply_finalized(2, block_hash, root)
        .map_err(|error| format!("cold finalized apply: {error}"))?;
    let elapsed = started.elapsed();
    let record = serde_json::json!({
        "case": text("case")?,
        "cache_state": "operator-attested-cold-after-reviewed-external-drop",
        "samples": 1,
        "includes_environment_open": true,
        "elapsed_ns": u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
        "dataset_checksum": format!("{:#x}", workload.dataset_checksum),
        "expected_root": format!("{:#x}", candidate.new_root()),
        "batch_checksum": format!("{:#x}", candidate_checksum(&candidate)),
        "staged_bytes": candidate.encoded_size(),
        "changed_shards": candidate.changed_shard_count(),
        "branch_records": candidate.branch_change_count(),
        "leaf_records": candidate.leaf_change_count(),
        "mdbx_bytes_before": mdbx_bytes_before,
        "mdbx_bytes_after": directory_bytes(&directory),
        "mdbx_allocated_bytes_before": mdbx_allocated_bytes_before,
        "mdbx_allocated_bytes_after": directory_allocated_bytes(&directory),
    });
    println!("ADR009_COLD_MANIFEST {record}");
    Ok(())
}
