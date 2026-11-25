//! Build deterministic sparse raw data disk images from VM network artifacts.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use eyre::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use outbe_operator_node_role::progress::Progress;

use crate::{
    network::{
        build::{DeploymentKind, FounderArtifact},
        remote::NetworkArtifacts,
    },
    vm::container::{self, DiskIds, FsEntry, Tools},
};

const DATA_ROOT_IN_FS: &str = "outbe-chain";
const GIB: u64 = 1024 * 1024 * 1024;

/// Build a deterministic ext4 data disk image for one VM founder.
pub fn run(
    artifacts: &Path,
    founder_selector: &str,
    output: &Path,
    size_gib: Option<u32>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("loading VM network artifacts");
    ensure!(
        !output.exists(),
        "output disk image already exists: {}",
        output.display()
    );
    let build = NetworkArtifacts::load(artifacts)?;
    ensure!(
        build.manifest.deployment == DeploymentKind::Vm,
        "network artifacts use bare-host deployment, not vm"
    );
    let founder = select_founder(&build.manifest.founders, founder_selector)?;
    let vm = founder
        .vm
        .as_ref()
        .ok_or_else(|| eyre::eyre!("founder {} manifest has no vm metadata", founder.name))?;
    let disk_gib = size_gib.unwrap_or(vm.disk_gib);
    ensure!(disk_gib > 0, "disk size must be positive");
    let disk_bytes = u64::from(disk_gib) * GIB;
    let source_tree = build.root.join(&vm.data_tree);
    ensure!(
        source_tree.is_dir(),
        "founder VM data tree does not exist: {}",
        source_tree.display()
    );

    progress.stage(&format!(
        "building {disk_gib} GiB data disk for {}",
        founder.name
    ));
    let tools = Tools::resolve()?;
    build_disk_from_tree(&tools, &source_tree, output, disk_bytes)?;
    progress.done(&format!("wrote VM data disk to {}", output.display()));
    Ok(())
}

fn build_disk_from_tree(
    tools: &Tools,
    source_tree: &Path,
    output: &Path,
    disk_bytes: u64,
) -> Result<()> {
    ensure!(
        !output.exists(),
        "output disk image already exists: {}",
        output.display()
    );
    let (entries, content_digest) = collect_entries(source_tree)?;
    let ids = derive_disk_ids(&content_digest);
    container::write_disk_image(tools, output, disk_bytes, &ids, &entries)
}

fn select_founder<'a>(
    founders: &'a [FounderArtifact],
    selector: &str,
) -> Result<&'a FounderArtifact> {
    if let Ok(index) = selector.parse::<usize>() {
        if let Some(founder) = founders.iter().find(|item| item.index == index) {
            return Ok(founder);
        }
    }
    if let Some(founder) = founders.iter().find(|item| item.name == selector) {
        return Ok(founder);
    }
    bail!("unknown founder '{selector}' in network manifest");
}

/// Walk `source_tree` in deterministic lexicographic order, rejecting symlinks,
/// non-regular files, and executable files, and return `(entries, content_sha256)`.
fn collect_entries(source_tree: &Path) -> Result<(Vec<FsEntry>, [u8; 32])> {
    let mut dirs: Vec<(String, u32)> = Vec::new();
    let mut files: Vec<(String, PathBuf, u32)> = Vec::new();

    let root_meta = fs::symlink_metadata(source_tree)
        .wrap_err_with(|| format!("failed to stat {}", source_tree.display()))?;
    ensure!(
        root_meta.is_dir() && !root_meta.file_type().is_symlink(),
        "VM data tree is not a directory: {}",
        source_tree.display()
    );
    dirs.push((
        DATA_ROOT_IN_FS.to_owned(),
        root_meta.permissions().mode() & 0o777,
    ));

    for entry in WalkDir::new(source_tree).follow_links(false) {
        let entry = entry?;
        let path = entry.path();
        if path == source_tree {
            continue;
        }
        let rel = path.strip_prefix(source_tree)?;
        let rel_str = rel
            .to_str()
            .ok_or_else(|| eyre::eyre!("non-UTF-8 path in VM data tree: {}", path.display()))?;
        let target = format!("{DATA_ROOT_IN_FS}/{rel_str}");
        let meta = fs::symlink_metadata(path)?;
        let ft = meta.file_type();
        ensure!(
            !ft.is_symlink(),
            "refusing symlink in VM data tree: {}",
            path.display()
        );
        let mode = meta.permissions().mode() & 0o7777;
        if ft.is_dir() {
            dirs.push((target, mode & 0o777));
        } else if ft.is_file() {
            ensure!(
                mode & 0o111 == 0,
                "refusing executable file in VM data tree (mode {mode:04o}): {}",
                path.display()
            );
            ensure!(
                mode & 0o6000 == 0,
                "refusing setuid/setgid file in VM data tree (mode {mode:04o}): {}",
                path.display()
            );
            files.push((target, path.to_path_buf(), mode & 0o777));
        } else {
            bail!(
                "refusing non-regular entry in VM data tree: {}",
                path.display()
            );
        }
    }

    dirs.sort_by(|left, right| left.0.cmp(&right.0));
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut hasher = Sha256::new();
    let mut entries = Vec::with_capacity(dirs.len() + files.len());
    for (target, mode) in dirs {
        hasher.update(b"D\0");
        hasher.update(target.as_bytes());
        hasher.update(b"\0");
        hasher.update(mode.to_le_bytes());
        entries.push(FsEntry::Dir { target, mode });
    }
    for (target, source, mode) in files {
        let bytes =
            fs::read(&source).wrap_err_with(|| format!("failed to read {}", source.display()))?;
        hasher.update(b"F\0");
        hasher.update(target.as_bytes());
        hasher.update(b"\0");
        hasher.update(mode.to_le_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
        entries.push(FsEntry::File {
            source,
            target,
            mode,
        });
    }

    Ok((entries, hasher.finalize().into()))
}

fn derive_disk_ids(content_digest: &[u8; 32]) -> DiskIds {
    DiskIds {
        disk_guid: derive_uuid(content_digest, b"outbe-vm-disk-guid"),
        part_guid: derive_uuid(content_digest, b"outbe-vm-part-guid"),
        fs_uuid: derive_uuid(content_digest, b"outbe-vm-fs-uuid"),
        hash_seed: derive_uuid(content_digest, b"outbe-vm-hash-seed"),
    }
}

fn derive_uuid(seed: &[u8; 32], domain: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(b"\0");
    hasher.update(seed);
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // Set RFC 4122 version 4 and variant 10xx bits.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Seek, SeekFrom},
        process::Command,
    };

    use outbe_software_bundle::sha256_file;

    use super::*;
    use crate::vm::container::{PARTITION_LABEL, PARTITION_OFFSET_BYTES};

    const TEST_DISK_BYTES: u64 = 16 * 1024 * 1024; // 16 MiB sparse image for fast unit tests.

    fn write_file_mode(path: &Path, content: &[u8], mode: u32) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn build_sample_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
        write_file_mode(
            &root.join("genesis.json"),
            br#"{"config":{"chainId":424242}}"#,
            0o644,
        );
        write_file_mode(&root.join(".bb-crs/bn254_g1.dat"), b"crs-data", 0o644);
        write_file_mode(
            &root.join("validator.env"),
            b"OUTBE_CHAIN_ID=\"424242\"\n",
            0o640,
        );
        write_file_mode(&root.join("feeder-public.toml"), b"[oracle]\n", 0o640);
        write_file_mode(&root.join("hyperlane-public.toml"), b"[hyperlane]\n", 0o640);
        fs::create_dir_all(root.join("keys/radicle/keys")).unwrap();
        fs::set_permissions(root.join("keys"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(root.join("keys/radicle"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(
            root.join("keys/radicle/keys"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let key_hex = "a".repeat(64);
        for rel in [
            "keys/signing-key.hex",
            "keys/evm-key.hex",
            "keys/feeder-key.hex",
            "keys/hyperlane-key.hex",
            "keys/reth-p2p-secret.hex",
            "keys/radicle/keys/radicle",
        ] {
            write_file_mode(&root.join(rel), key_hex.as_bytes(), 0o600);
        }
        dir
    }

    fn extract_partition(disk: &Path, out: &Path, disk_bytes: u64) {
        let (_, part_bytes) = container::partition_geometry(disk_bytes).unwrap();
        let mut src = fs::File::open(disk).unwrap();
        src.seek(SeekFrom::Start(PARTITION_OFFSET_BYTES)).unwrap();
        let mut reader = src.take(part_bytes);
        let mut dst = fs::File::create(out).unwrap();
        std::io::copy(&mut reader, &mut dst).unwrap();
    }

    #[test]
    fn disk_image_is_deterministic_has_dstack_gpt_and_root_owned_files() {
        let Ok(tools) = Tools::resolve() else {
            eprintln!("SKIPPED: sfdisk/mke2fs/debugfs not available in PATH");
            return;
        };

        let tree = build_sample_tree();
        let out_dir = tempfile::tempdir().unwrap();
        let img_a = out_dir.path().join("a.img");
        let img_b = out_dir.path().join("b.img");

        build_disk_from_tree(&tools, tree.path(), &img_a, TEST_DISK_BYTES).unwrap();
        build_disk_from_tree(&tools, tree.path(), &img_b, TEST_DISK_BYTES).unwrap();

        // 1. Determinism: identical bytes across repeated builds.
        assert_eq!(sha256_file(&img_a).unwrap(), sha256_file(&img_b).unwrap());

        // 2. GPT partition table has PARTLABEL=dstack-data.
        let sfdisk_out = Command::new(&tools.sfdisk)
            .args(["-d"])
            .arg(&img_a)
            .output()
            .unwrap();
        assert!(sfdisk_out.status.success());
        let dump = String::from_utf8_lossy(&sfdisk_out.stdout);
        assert!(
            dump.contains(&format!("name=\"{PARTITION_LABEL}\"")),
            "missing dstack-data PARTLABEL in sfdisk dump:\n{dump}"
        );

        // 3. Read back ext4 metadata without mounting via debugfs and verify no exec bits.
        let part_file = out_dir.path().join("part.ext4");
        extract_partition(&img_a, &part_file, TEST_DISK_BYTES);

        for (path, expected_mode, is_file) in [
            ("outbe-chain", "0755", false),
            ("outbe-chain/genesis.json", "0644", true),
            ("outbe-chain/.bb-crs/bn254_g1.dat", "0644", true),
            ("outbe-chain/validator.env", "0640", true),
            ("outbe-chain/feeder-public.toml", "0640", true),
            ("outbe-chain/hyperlane-public.toml", "0640", true),
            ("outbe-chain/keys", "0700", false),
            ("outbe-chain/keys/signing-key.hex", "0600", true),
            ("outbe-chain/keys/evm-key.hex", "0600", true),
            ("outbe-chain/keys/feeder-key.hex", "0600", true),
            ("outbe-chain/keys/hyperlane-key.hex", "0600", true),
            ("outbe-chain/keys/reth-p2p-secret.hex", "0600", true),
            ("outbe-chain/keys/radicle/keys/radicle", "0600", true),
        ] {
            let stat_out = Command::new(&tools.debugfs)
                .args(["-R", &format!("stat {path}")])
                .arg(&part_file)
                .output()
                .unwrap();
            assert!(stat_out.status.success());
            let stat = String::from_utf8_lossy(&stat_out.stdout);
            assert!(
                stat.contains("User:     0") && stat.contains("Group:     0"),
                "{path} is not owned by 0:0:\n{stat}"
            );
            assert!(
                stat.contains(&format!("Mode:  {expected_mode}")),
                "{path} mode mismatch (expected {expected_mode}):\n{stat}"
            );
            if is_file {
                let parsed_mode = u32::from_str_radix(expected_mode, 8).unwrap();
                assert_eq!(parsed_mode & 0o111, 0, "{path} must not be executable");
            }
        }

        // 4. Verify e2fsck passes cleanly when available.
        if let Ok(status) = Command::new("e2fsck")
            .args(["-f", "-n"])
            .arg(&part_file)
            .status()
        {
            assert!(status.success(), "e2fsck -f -n failed with {status}");
        }

        // 5. Refuses to overwrite an existing output file.
        let err = build_disk_from_tree(&tools, tree.path(), &img_a, TEST_DISK_BYTES).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn collect_entries_rejects_executable_files() {
        let tree = build_sample_tree();
        write_file_mode(&tree.path().join("rogue.sh"), b"#!/bin/sh\n", 0o755);
        let err = collect_entries(tree.path()).unwrap_err();
        assert!(
            err.to_string().contains("refusing executable file"),
            "{err}"
        );
    }

    #[test]
    fn refuses_non_vm_manifest_and_existing_output() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("exists.img");
        fs::write(&existing, b"existing").unwrap();
        let err = run(temp.path(), "0", &existing, None).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");

        // Validate that a bare-host deployment kind is rejected.
        let artifacts = temp.path().join("host-artifacts");
        fs::create_dir_all(artifacts.join("dist")).unwrap();
        fs::write(
            artifacts.join("network.yaml"),
            crate::network::config::EXAMPLE,
        )
        .unwrap();
        fs::write(artifacts.join("software-bundle.tar.zst"), b"bundle").unwrap();
        let hash = alloy_primitives::B256::repeat_byte(0x42);
        let policy = outbe_primitives::tee_genesis_v1::initial_tee_policy_v1(
            outbe_primitives::tee_genesis_v1::InitialTeeProfileV1::NoAttestationDev,
            54_322_345,
            hash,
        )
        .unwrap();
        let genesis = serde_json::json!({
            "config": {
                "chainId": 54_322_345,
                "teeAttestationV1": outbe_primitives::tee_genesis_v1::tee_attestation_v1_genesis_field(&policy).unwrap()
            }
        });
        fs::write(
            artifacts.join("genesis.json"),
            serde_json::to_vec(&genesis).unwrap(),
        )
        .unwrap();
        let mut founders = Vec::new();
        for index in 0..4 {
            let rel = format!("dist/validator-{index}.tgz");
            fs::write(artifacts.join(&rel), b"tgz").unwrap();
            founders.push(serde_json::json!({
                "index": index,
                "name": format!("validator-{index}"),
                "ssh_host": format!("192.0.2.{}", index + 10),
                "advertise_ip": format!("10.0.0.{}", index + 10),
                "validator_address": "0x1111111111111111111111111111111111111111",
                "hyperlane_address": "0x2222222222222222222222222222222222222222",
                "consensus_public_key": "aa",
                "archive": rel,
                "archive_sha256": sha256_file(&artifacts.join(format!("dist/validator-{index}.tgz"))).unwrap(),
            }));
        }
        let manifest = serde_json::json!({
            "version": 1,
            "state": "ready",
            "deployment": "host",
            "name": "local-testnet",
            "network": "testnet",
            "chain_id": 54_322_345,
            "config_sha256": sha256_file(&artifacts.join("network.yaml")).unwrap(),
            "software": {
                "archive_sha256": sha256_file(&artifacts.join("software-bundle.tar.zst")).unwrap(),
                "commit": "abc",
                "release_tag": "v0",
                "target": "x86_64-unknown-linux-gnu",
            },
            "genesis": {
                "seeded_path": "genesis.seeded.json",
                "seeded_sha256": "00",
                "final_path": "genesis.json",
                "final_sha256": sha256_file(&artifacts.join("genesis.json")).unwrap(),
                "genesis_block_hash": format!("{hash:#x}"),
            },
            "founders": founders,
            "joining": {
                "bundle": "joining/bundle.tar.zst",
                "bundle_sha256": "00",
                "config_example": "joining/operator-config.example.yaml",
            }
        });
        fs::write(
            artifacts.join("network-manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let output = temp.path().join("out.img");
        let err = run(&artifacts, "0", &output, None).unwrap_err();
        assert!(
            err.to_string().contains("bare-host deployment, not vm"),
            "{err}"
        );
    }
}
