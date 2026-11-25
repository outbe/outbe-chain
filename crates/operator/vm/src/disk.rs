//! Deterministic sparse raw data disk builder and sparse-transfer helpers.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use eyre::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::{
    container::{self, DiskIds, FsEntry, Tools},
    shell_quote,
    vmm::remote_vmm_path,
};

const DATA_ROOT_IN_FS: &str = "outbe-chain";

/// Build a deterministic sparse raw GPT + ext4 data disk image from `source_tree`.
pub fn build_disk_image(source_tree: &Path, output: &Path, disk_bytes: u64) -> Result<()> {
    let tools = Tools::resolve()?;
    build_disk_from_tree(&tools, source_tree, output, disk_bytes)
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

/// Build the host shell command that extracts a sparse raw disk, verifies its raw SHA-256,
/// converts it to qcow2, resizes to `disk_gib`, and atomically publishes `hda.img`.
pub fn build_disk_upload_and_convert_command(
    vmm_home: &str,
    vm_id: &str,
    filename: &str,
    expected_sha256: &str,
    disk_gib: u32,
) -> String {
    let vm_dir_expr = remote_vmm_path(vmm_home, &format!("vm/{vm_id}"));
    let qcow2_expr = remote_vmm_path(vmm_home, &format!("vm/{vm_id}/hda.img"));
    let filename_quoted = shell_quote(filename);
    let expected_quoted = shell_quote(expected_sha256);
    format!(
        "set -e; \
         mkdir -p {vm_dir_expr}; \
         tmp_dir=$(mktemp -d {vm_dir_expr}/.hda-upload.XXXXXX); \
         trap 'rm -rf \"$tmp_dir\"' EXIT; \
         tar --sparse -xzf - -C \"$tmp_dir\"; \
         raw=\"$tmp_dir\"/{filename_quoted}; \
         if [ ! -f \"$raw\" ]; then echo 'uploaded raw disk missing' >&2; exit 1; fi; \
         actual=$(sha256sum \"$raw\" | awk '{{print $1}}'); \
         if [ \"$actual\" != {expected_quoted} ]; then echo \"uploaded raw disk checksum mismatch: $actual\" >&2; exit 1; fi; \
         qcow2_tmp=\"$tmp_dir/hda.img\"; \
         qemu-img convert -f raw -O qcow2 \"$raw\" \"$qcow2_tmp\"; \
         rm -f \"$raw\"; \
         qemu-img resize -f qcow2 \"$qcow2_tmp\" {disk_gib}G; \
         mv -f \"$qcow2_tmp\" {qcow2_expr}"
    )
}

/// Stream `tar --sparse -czf - -C <base_dir> <entry>` into `sink` without reading holes.
pub fn stream_sparse_tar_gz(
    base_dir: &Path,
    entry: &str,
    sink: &mut dyn std::io::Write,
) -> Result<()> {
    use std::process::{Command, Stdio};

    let mut child = Command::new("tar")
        .args(["--sparse", "-czf", "-", "-C"])
        .arg(base_dir)
        .arg(entry)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err("spawn local tar --sparse -czf")?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| eyre::eyre!("tar stdout unavailable"))?;
    let copy_result = std::io::copy(&mut stdout, sink);
    let output = child.wait_with_output().wrap_err("wait for local tar")?;
    if !output.status.success() {
        bail!(
            "local tar --sparse failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    copy_result.wrap_err("stream compressed sparse tar to SSH")?;
    Ok(())
}

/// Hash the logical bytes of a sparse file using `SEEK_DATA`/`SEEK_HOLE` to skip reading holes.
pub fn sha256_sparse_file(path: &Path) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};

    use rustix::fs::SeekFrom as RustixSeekFrom;

    let mut file =
        fs::File::open(path).wrap_err_with(|| format!("open sparse file: {}", path.display()))?;
    let file_len = file.metadata()?.len();
    let mut hasher = Sha256::new();
    let zero_chunk = [0_u8; 64 * 1024];
    let mut read_buf = vec![0_u8; 64 * 1024];

    let feed_zeros = |hasher: &mut Sha256, mut count: u64| {
        while count > 0 {
            let step = (count as usize).min(zero_chunk.len());
            hasher.update(&zero_chunk[..step]);
            count -= step as u64;
        }
    };

    let mut pos: u64 = 0;
    while pos < file_len {
        let data_start = match rustix::fs::seek(&file, RustixSeekFrom::Data(pos)) {
            Ok(offset) => offset,
            Err(rustix::io::Errno::NXIO) => {
                // Remaining bytes through EOF are a hole.
                feed_zeros(&mut hasher, file_len - pos);
                break;
            }
            Err(rustix::io::Errno::INVAL | rustix::io::Errno::OPNOTSUPP) => {
                // Filesystem does not support SEEK_DATA; fall back to linear read.
                file.seek(SeekFrom::Start(pos))?;
                let mut remaining = file_len - pos;
                while remaining > 0 {
                    let want = (remaining as usize).min(read_buf.len());
                    let read = file.read(&mut read_buf[..want])?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&read_buf[..read]);
                    remaining -= read as u64;
                }
                break;
            }
            Err(err) => return Err(err).wrap_err("SEEK_DATA on sparse disk"),
        };

        if data_start > pos {
            feed_zeros(&mut hasher, data_start - pos);
        }

        let data_end = match rustix::fs::seek(&file, RustixSeekFrom::Hole(data_start)) {
            Ok(offset) => offset.min(file_len),
            Err(rustix::io::Errno::NXIO) => file_len,
            Err(err) => return Err(err).wrap_err("SEEK_HOLE on sparse disk"),
        };

        file.seek(SeekFrom::Start(data_start))?;
        let mut remaining = data_end - data_start;
        while remaining > 0 {
            let want = (remaining as usize).min(read_buf.len());
            let read = file.read(&mut read_buf[..want])?;
            ensure!(read > 0, "unexpected EOF while hashing sparse disk");
            hasher.update(&read_buf[..read]);
            remaining -= read as u64;
        }
        pos = data_end;
    }

    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Seek, SeekFrom},
        process::Command,
    };

    use outbe_software_bundle::sha256_file;

    use super::*;
    use crate::container::{PARTITION_LABEL, PARTITION_OFFSET_BYTES};

    const TEST_DISK_BYTES: u64 = 16 * 1024 * 1024; // 16 MiB sparse image for fast unit tests.
    const UUID_0: &str = "550e8400-e29b-41d4-a716-446655440000";
    const UUID_1: &str = "550e8400-e29b-41d4-a716-446655440001";

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
    fn sha256_sparse_file_matches_logical_zero_filled_sha256() {
        use std::{
            io::{Seek, SeekFrom, Write},
            process::Command,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sparse.img");
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let logical_size: u64 = 16 * 1024 * 1024;
        file.set_len(logical_size).unwrap();
        file.write_all(b"header-bytes-at-zero").unwrap();
        file.seek(SeekFrom::Start(8 * 1024 * 1024)).unwrap();
        file.write_all(b"middle-extent-payload").unwrap();
        file.seek(SeekFrom::Start(logical_size - 16)).unwrap();
        file.write_all(b"tail-16-bytes!!!").unwrap();
        file.sync_all().unwrap();
        drop(file);

        let fast_hash = sha256_sparse_file(&path).unwrap();
        let out = Command::new("sha256sum").arg(&path).output().unwrap();
        assert!(out.status.success());
        let expected = String::from_utf8(out.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
        assert_eq!(fast_hash, expected);
    }

    #[test]
    fn sparse_disk_stream_and_verify_executes_locally() {
        use std::{
            io::{Seek, SeekFrom, Write},
            os::unix::fs::PermissionsExt,
            process::{Command, Stdio},
        };

        let dir = tempfile::tempdir().unwrap();
        let shim_dir = dir.path().join("bin");
        fs::create_dir_all(&shim_dir).unwrap();
        let qemu_log = dir.path().join("qemu-img.log");
        let qemu_shim = shim_dir.join("qemu-img");
        // Shim records calls and copies raw->qcow2 on `convert`.
        fs::write(
            &qemu_shim,
            format!(
                "#!/bin/sh\nset -e\necho \"$*\" >> {}\nif [ \"$1\" = \"convert\" ]; then cp --sparse=always \"$6\" \"$7\"; fi\n",
                shell_quote(&qemu_log.to_string_lossy())
            ),
        )
        .unwrap();
        fs::set_permissions(&qemu_shim, fs::Permissions::from_mode(0o755)).unwrap();

        let path_env = format!(
            "{}:{}",
            shim_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );

        // Create a 32 MiB sparse raw disk with only a few bytes of actual data.
        let src_disk = dir.path().join("validator-0.img");
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&src_disk)
            .unwrap();
        let logical_size: u64 = 32 * 1024 * 1024;
        file.set_len(logical_size).unwrap();
        file.write_all(b"outbe-gpt-header").unwrap();
        file.seek(SeekFrom::Start(16 * 1024 * 1024)).unwrap();
        file.write_all(b"outbe-ext4-data").unwrap();
        file.sync_all().unwrap();
        drop(file);

        let expected_sha = sha256_sparse_file(&src_disk).unwrap();

        // Compress sparse stream into memory to confirm wire size is tiny compared to 32 MiB.
        let mut wire_bytes = Vec::new();
        stream_sparse_tar_gz(dir.path(), "validator-0.img", &mut wire_bytes).unwrap();
        assert!(
            wire_bytes.len() < 64 * 1024,
            "compressed sparse stream should be tiny, got {} bytes",
            wire_bytes.len()
        );

        let vmm_home = dir.path().join("vmm");
        let cmd = build_disk_upload_and_convert_command(
            vmm_home.to_str().unwrap(),
            UUID_0,
            "validator-0.img",
            &expected_sha,
            40,
        );
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .env("PATH", &path_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&wire_bytes).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "disk upload/convert failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let published = vmm_home.join(format!("vm/{UUID_0}/hda.img"));
        assert!(published.is_file());
        let log = fs::read_to_string(&qemu_log).unwrap();
        assert!(log.contains("convert -f raw -O qcow2"));
        assert!(log.contains("resize -f qcow2"));
        assert!(log.contains("40G"));

        // Mismatched SHA-256 fails closed BEFORE invoking qemu-img.
        let bad_log = dir.path().join("qemu-img-bad.log");
        fs::write(
            &qemu_shim,
            format!(
                "#!/bin/sh\necho \"$*\" >> {}\n",
                shell_quote(&bad_log.to_string_lossy())
            ),
        )
        .unwrap();
        let bad_cmd = build_disk_upload_and_convert_command(
            vmm_home.to_str().unwrap(),
            UUID_1,
            "validator-0.img",
            &"0".repeat(64),
            40,
        );
        let mut bad_child = Command::new("sh")
            .arg("-c")
            .arg(&bad_cmd)
            .env("PATH", &path_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        bad_child
            .stdin
            .take()
            .unwrap()
            .write_all(&wire_bytes)
            .unwrap();
        let bad_out = bad_child.wait_with_output().unwrap();
        assert!(!bad_out.status.success(), "bad checksum must fail closed");
        assert!(
            !bad_log.exists(),
            "qemu-img must not run when raw checksum mismatches"
        );
    }
}
