use std::os::unix::fs::PermissionsExt;

use super::extract::{verify_archive_path_and_extract, ExtractJob, ExtractLimits};
use super::*;

fn write_file_mode(path: &Path, bytes: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn build_sample_image_dir(dir: &Path, digest: &str) {
    write_file_mode(
        &dir.join("digest.txt"),
        format!("{digest}\n").as_bytes(),
        0o644,
    );
    write_file_mode(&dir.join("ovmf.fd"), b"fake-bios-bytes", 0o644);
    write_file_mode(&dir.join("bzImage"), b"fake-kernel-bytes", 0o644);
    write_file_mode(&dir.join("initramfs.cpio.gz"), b"fake-initrd", 0o644);
    write_file_mode(&dir.join("rootfs.squashfs"), b"fake-rootfs-payload", 0o644);
    write_file_mode(
        &dir.join("metadata.json"),
        br#"{"bios":"ovmf.fd","kernel":"bzImage","initrd":"initramfs.cpio.gz","rootfs":"rootfs.squashfs"}"#,
        0o644,
    );
    // Build-time-only disk.raw must be excluded from the distribution archive.
    write_file_mode(&dir.join("disk.raw"), b"huge-raw-disk-excluded", 0o644);
    let rootfs_sha = sha256_file_streaming(&dir.join("rootfs.squashfs")).unwrap();
    write_file_mode(
        &dir.join("sha256sum.txt"),
        format!("{rootfs_sha}  rootfs.squashfs\n").as_bytes(),
        0o644,
    );
}

#[test]
fn pack_verify_extract_round_trip_is_deterministic_and_excludes_disk_raw() {
    let tmp = tempfile::tempdir().unwrap();
    let digest = "0123456789abcdef".repeat(4);
    let src = tmp.path().join("src-image");
    build_sample_image_dir(&src, &digest);

    let meta = ArchiveBuildMetadata {
        flavor: Flavor::Dev,
        source_commit: "a".repeat(40).leak(),
        dstack_rev: "b".repeat(40).leak(),
        overlay_commit: "c".repeat(40).leak(),
    };

    let archive_a = tmp.path().join("a.tar.zst");
    let archive_b = tmp.path().join("b.tar.zst");
    let (manifest_a, sha_a) = pack_flavor_archive(&src, &meta, &archive_a).unwrap();
    let (manifest_b, sha_b) = pack_flavor_archive(&src, &meta, &archive_b).unwrap();
    assert_eq!(manifest_a, manifest_b);
    assert_eq!(sha_a, sha_b, "archive packing must be byte-identical");
    assert!(!manifest_a.files.iter().any(|f| f.path == "disk.raw"));

    let extracted = tmp.path().join("extracted");
    fs::create_dir(&extracted).unwrap();
    let verified_manifest =
        verify_archive_and_extract(&archive_a, &sha_a, Some(&digest), &extracted).unwrap();
    assert_eq!(verified_manifest, manifest_a);
    assert!(!extracted.join("disk.raw").exists());
    assert!(extracted.join(ARCHIVE_MANIFEST_NAME).is_file());

    // Offline verify_image_dir succeeds using embedded image-archive-manifest.json.
    let verified_dir = verify_image_dir(&extracted, None, Some(Flavor::Dev)).unwrap();
    assert_eq!(verified_dir.os_image_hash, digest);
    assert_eq!(verified_dir.flavor, Some(Flavor::Dev));
    assert_eq!(verified_dir.verified_files, manifest_a.files.len());

    // Tampering with one byte of rootfs fails verify_image_dir.
    write_file_mode(
        &extracted.join("rootfs.squashfs"),
        b"tampered-rootfs",
        0o644,
    );
    let err = verify_image_dir(&extracted, None, None).expect_err("tampered file must fail");
    assert!(
        err.to_string().contains("sha256sum.txt mismatch")
            || err.to_string().contains("SHA-256 mismatch"),
        "{err}"
    );
}

#[test]
fn verify_archive_rejects_hash_mismatch_before_decompression_and_caps_stream_and_files() {
    let tmp = tempfile::tempdir().unwrap();
    let garbage = tmp.path().join("corrupt.tar.zst");
    fs::write(&garbage, b"not-valid-zstd-at-all").unwrap();
    let out = tmp.path().join("out");
    fs::create_dir(&out).unwrap();

    // 1. Verify-before-parse: wrong SHA-256 fails on SHA-256 mismatch, NOT zstd decode error.
    let err = verify_archive_and_extract(&garbage, &"0".repeat(64), None, &out)
        .expect_err("wrong archive sha256 must fail before decompression");
    assert!(
        err.to_string().contains("archive SHA-256 mismatch"),
        "{err}"
    );

    // 2. Oversized PAX extended header (> 1 MiB) fails before tar allocates it even when the
    // outer archive SHA-256 matches.
    let pax_bomb = tmp.path().join("pax-bomb.tar.zst");
    {
        let file = File::create(&pax_bomb).unwrap();
        let mut enc = zstd::Encoder::new(file, 3).unwrap();
        let pax_size: u64 = 2 * 1024 * 1024;
        let mut h = tar::Header::new_ustar();
        h.set_entry_type(tar::EntryType::XHeader);
        h.set_mode(0o644);
        h.set_size(pax_size);
        h.set_path("PaxHeaders.0/huge").unwrap();
        h.set_cksum();
        enc.write_all(h.as_bytes()).unwrap();
        let chunk = vec![b'x'; 64 * 1024];
        let mut rem = pax_size as usize;
        while rem > 0 {
            let n = rem.min(chunk.len());
            enc.write_all(&chunk[..n]).unwrap();
            rem -= n;
        }
        enc.finish().unwrap();
    }
    let pax_sha = sha256_file_streaming(&pax_bomb).unwrap();
    let pax_out = tmp.path().join("pax-out");
    fs::create_dir(&pax_out).unwrap();
    let err = verify_archive_and_extract(&pax_bomb, &pax_sha, None, &pax_out)
        .expect_err("PAX header bomb must be rejected before allocation");
    assert!(
        format!("{err:#}").contains("entry header/metadata exceeds maximum"),
        "{err:#}"
    );

    // 3. File count cap and extracted byte cap are enforced.
    let digest = "fedcba9876543210".repeat(4);
    let src = tmp.path().join("src2");
    build_sample_image_dir(&src, &digest);
    let meta = ArchiveBuildMetadata {
        flavor: Flavor::Prod,
        source_commit: "a".repeat(40).leak(),
        dstack_rev: "b".repeat(40).leak(),
        overlay_commit: "c".repeat(40).leak(),
    };
    let valid_archive = tmp.path().join("valid.tar.zst");
    let (_, valid_sha) = pack_flavor_archive(&src, &meta, &valid_archive).unwrap();

    let never = AtomicBool::new(false);
    let cap_files_out = tmp.path().join("cap-files");
    fs::create_dir(&cap_files_out).unwrap();
    let err = verify_archive_path_and_extract(
        &valid_archive,
        &ExtractJob {
            expected: ArchiveExpectation {
                archive_sha256: &valid_sha,
                os_image_hash: None,
            },
            dest_dir: &cap_files_out,
            limits: ExtractLimits {
                max_files: 2,
                ..ExtractLimits::default()
            },
            cancel: &never,
        },
        || Ok(()),
    )
    .expect_err("file count cap must be enforced");
    assert!(err.to_string().contains("exceeding limit 2"), "{err}");

    let cap_bytes_out = tmp.path().join("cap-bytes");
    fs::create_dir(&cap_bytes_out).unwrap();
    let err = verify_archive_path_and_extract(
        &valid_archive,
        &ExtractJob {
            expected: ArchiveExpectation {
                archive_sha256: &valid_sha,
                os_image_hash: None,
            },
            dest_dir: &cap_bytes_out,
            limits: ExtractLimits {
                max_extracted_bytes: 16,
                ..ExtractLimits::default()
            },
            cancel: &never,
        },
        || Ok(()),
    )
    .expect_err("extracted byte cap must be enforced");
    assert!(
        err.to_string()
            .contains("extracted image size exceeds maximum 16"),
        "{err}"
    );
}

#[test]
fn pack_refuses_existing_dangling_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let digest = "1111111111111111".repeat(4);
    let src = tmp.path().join("src");
    build_sample_image_dir(&src, &digest);
    let meta = ArchiveBuildMetadata {
        flavor: Flavor::Dev,
        source_commit: "a".repeat(40).leak(),
        dstack_rev: "b".repeat(40).leak(),
        overlay_commit: "c".repeat(40).leak(),
    };
    let dangling = tmp.path().join("dangling.tar.zst");
    std::os::unix::fs::symlink(tmp.path().join("missing-target"), &dangling).unwrap();
    let err = pack_flavor_archive(&src, &meta, &dangling)
        .expect_err("dangling symlink output must be refused");
    assert!(err.to_string().contains("already exists"), "{err}");
}

#[test]
fn single_fd_verify_ignores_path_swap_between_hash_and_extract() {
    let tmp = tempfile::tempdir().unwrap();
    let digest = "2222222222222222".repeat(4);
    let src = tmp.path().join("src");
    build_sample_image_dir(&src, &digest);
    let meta = ArchiveBuildMetadata {
        flavor: Flavor::Dev,
        source_commit: "a".repeat(40).leak(),
        dstack_rev: "b".repeat(40).leak(),
        overlay_commit: "c".repeat(40).leak(),
    };
    let archive = tmp.path().join("legit.tar.zst");
    let (_, legit_sha) = pack_flavor_archive(&src, &meta, &archive).unwrap();

    // Prepare an attacker file on disk to atomically swap into `archive` right after the
    // hash phase and BEFORE extraction.
    let evil_tmp = tmp.path().join("evil.tar.zst");
    fs::write(&evil_tmp, b"ATTACKER-SWAPPED-NON-ZSTD-PAYLOAD").unwrap();

    let dest = tmp.path().join("extracted");
    fs::create_dir(&dest).unwrap();
    let never = AtomicBool::new(false);
    let archive_for_swap = archive.clone();
    let evil_for_swap = evil_tmp.clone();

    let manifest = verify_archive_path_and_extract(
        &archive,
        &ExtractJob {
            expected: ArchiveExpectation {
                archive_sha256: &legit_sha,
                os_image_hash: Some(&digest),
            },
            dest_dir: &dest,
            limits: ExtractLimits::default(),
            cancel: &never,
        },
        move || {
            // Atomically swap the directory entry to a brand-new inode after hashing!
            fs::rename(&evil_for_swap, &archive_for_swap)?;
            Ok(())
        },
    )
    .expect("single-FD extraction must read verified original FD despite path swap");

    assert_eq!(manifest.os_image_hash, digest);
    assert_eq!(
        fs::read_to_string(dest.join("digest.txt")).unwrap().trim(),
        digest
    );
    // Confirm the path on disk really was replaced with the attacker payload.
    assert_eq!(
        fs::read(&archive).unwrap(),
        b"ATTACKER-SWAPPED-NON-ZSTD-PAYLOAD"
    );
}

#[test]
fn verify_archive_refuses_symlink_directory_and_fifo_without_blocking() {
    use std::process::Command;
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().unwrap();
    let digest = "3333333333333333".repeat(4);
    let src = tmp.path().join("src");
    build_sample_image_dir(&src, &digest);
    let meta = ArchiveBuildMetadata {
        flavor: Flavor::Dev,
        source_commit: "a".repeat(40).leak(),
        dstack_rev: "b".repeat(40).leak(),
        overlay_commit: "c".repeat(40).leak(),
    };
    let real_archive = tmp.path().join("real.tar.zst");
    let (_, sha) = pack_flavor_archive(&src, &meta, &real_archive).unwrap();

    let dest = tmp.path().join("out");
    fs::create_dir(&dest).unwrap();

    // 1. Symlink to a valid archive -> refused via O_NOFOLLOW.
    let link = tmp.path().join("link.tar.zst");
    std::os::unix::fs::symlink(&real_archive, &link).unwrap();
    let err = verify_archive_and_extract(&link, &sha, None, &dest)
        .expect_err("symlink archive must be refused");
    assert!(format!("{err:#}").contains("symlink"), "{err:#}");

    // 2. Directory -> refused via fstat !is_file().
    let dir = tmp.path().join("dir.tar.zst");
    fs::create_dir(&dir).unwrap();
    let err = verify_archive_and_extract(&dir, &sha, None, &dest)
        .expect_err("directory archive must be refused");
    assert!(format!("{err:#}").contains("non-regular"), "{err:#}");

    // 3. FIFO -> refused immediately (O_NONBLOCK + fstat !is_file()) without hanging!
    let fifo = tmp.path().join("pipe.tar.zst");
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let start = Instant::now();
    let err = verify_archive_and_extract(&fifo, &sha, None, &dest)
        .expect_err("FIFO archive must be refused without blocking");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "FIFO open took too long: {:?}",
        start.elapsed()
    );
    assert!(format!("{err:#}").contains("non-regular"), "{err:#}");
}
