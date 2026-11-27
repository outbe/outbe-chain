use std::process::Command;

use super::*;
use crate::{
    build_toolchain_docker_args, write_manifest, VmImageManifest, DSTACK_COMMIT, MANIFEST_NAME,
    MANIFEST_VERSION,
};

#[test]
fn docker_build_passes_load_and_repro_command_forwards_image_and_cpuset() {
    let args = build_toolchain_docker_args("outbe-toolchain:custom");
    assert!(args.contains(&"--load"));
    assert!(args
        .windows(2)
        .any(|w| w == ["--tag", "outbe-toolchain:custom"]));

    let cmd = build_repro_command(
        Path::new("/tmp/repro-build.sh"),
        Path::new("/tmp/out"),
        &ReproBuildSettings {
            flavors: &[Flavor::Dev],
            jobs: 4,
            mkosi_image: "outbe-mkosi:custom",
            cpuset_cpus: Some("0-3"),
        },
    );
    let cmd_args: Vec<_> = cmd.get_args().filter_map(|a| a.to_str()).collect();
    assert!(!cmd_args.contains(&"-c"));
    let envs: BTreeMap<_, _> = cmd
        .get_envs()
        .filter_map(|(k, v)| Some((k.to_str()?, v?.to_str()?)))
        .collect();
    assert_eq!(envs.get("FLAVORS"), Some(&"dev"));
    assert_eq!(envs.get("JOBS"), Some(&"4"));
    assert_eq!(envs.get("DSTACK_MKOSI_IMAGE"), Some(&"outbe-mkosi:custom"));
    assert_eq!(envs.get("DOCKER_CPUSET_CPUS"), Some(&"0-3"));
}

fn init_fake_overlay_repo(dir: &Path) -> String {
    fs::create_dir_all(dir).unwrap();
    run_git_in(dir, &["init", "--quiet"]).unwrap();
    fs::write(dir.join("marker.txt"), "overlay-v1\n").unwrap();
    run_git_in(dir, &["add", "-A"]).unwrap();
    let status = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "commit",
            "--quiet",
            "-m",
            "init",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    capture_git_in(dir, &["rev-parse", "HEAD"]).unwrap()
}

fn write_fake_flavor_output(
    build_dir: &Path,
    flavor: Flavor,
    version: &str,
    digest: &str,
    rootfs: &[u8],
) {
    let dir = build_dir.join(flavor_image_rel(&flavor, version));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("digest.txt"), format!("{digest}\n")).unwrap();
    fs::write(dir.join("rootfs.img"), rootfs).unwrap();
    fs::write(dir.join("metadata.json"), "{\"rootfs\":\"rootfs.img\"}\n").unwrap();
}

#[test]
fn repro_check_two_run_orchestration_passes_for_dev_and_prod_and_writes_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let work_dir = temp.path().join("work");
    let dstack_src = work_dir.join("dstack-outbe");
    let output_dir = temp.path().join("output");
    fs::create_dir_all(&output_dir).unwrap();
    let overlay_commit = init_fake_overlay_repo(&dstack_src);
    let dev_hash = "1".repeat(64);
    let prod_hash = "2".repeat(64);

    let mut seen_runs = Vec::new();
    let flavors = execute_repro_builds(
        &BuildDirs {
            work_dir: &work_dir,
            dstack_src: &dstack_src,
            output_dir: &output_dir,
        },
        &BuildIdentity {
            source_commit: &"a".repeat(40),
            dstack_rev: DSTACK_COMMIT,
            overlay_commit: &overlay_commit,
            dstack_version: "0.6.3",
            flavors: &[Flavor::Dev, Flavor::Prod],
        },
        true,
        |run_src, run_out| {
            assert_ne!(run_src, dstack_src);
            assert_eq!(
                capture_git_in(run_src, &["rev-parse", "HEAD"]).unwrap(),
                overlay_commit
            );
            seen_runs.push((run_src.to_path_buf(), run_out.to_path_buf()));
            write_fake_flavor_output(run_out, Flavor::Dev, "0.6.3", &dev_hash, b"dev-img");
            write_fake_flavor_output(run_out, Flavor::Prod, "0.6.3", &prod_hash, b"prod-img");
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(seen_runs.len(), 2);
    assert_ne!(seen_runs[0].0, seen_runs[1].0);
    assert_eq!(seen_runs[0].1, output_dir.join("a"));
    assert_eq!(seen_runs[1].1, output_dir.join("b"));
    assert!(output_dir
        .join("out/dev/dstack-dev-0.6.3/rootfs.img")
        .is_file());
    assert!(output_dir
        .join("out/prod/dstack-0.6.3/rootfs.img")
        .is_file());

    // Both runs produced deterministic archives and run a's archives were promoted to output_dir.
    let dev_archive = archive::archive_filename(Flavor::Dev, &dev_hash).unwrap();
    let prod_archive = archive::archive_filename(Flavor::Prod, &prod_hash).unwrap();
    assert!(output_dir.join(&dev_archive).is_file());
    assert!(output_dir.join(&prod_archive).is_file());
    assert!(output_dir.join("b").join(&dev_archive).is_file());
    assert_eq!(
        archive::sha256_file_streaming(&output_dir.join(&dev_archive)).unwrap(),
        archive::sha256_file_streaming(&output_dir.join("b").join(&dev_archive)).unwrap(),
        "run a and run b archives must have identical SHA-256"
    );

    let manifest = VmImageManifest {
        version: MANIFEST_VERSION,
        source_commit: "a".repeat(40),
        software_bundle_sha256: "d".repeat(64),
        dstack_rev: DSTACK_COMMIT.to_owned(),
        overlay_commit,
        reproducible: true,
        patches: Vec::new(),
        runtime_libs: Vec::new(),
        flavors,
    };
    write_manifest(&output_dir, &manifest).unwrap();
    let written: serde_json::Value =
        serde_json::from_slice(&fs::read(output_dir.join(MANIFEST_NAME)).unwrap()).unwrap();
    assert_eq!(written["reproducible"], serde_json::Value::Bool(true));
    assert_eq!(written["flavors"]["dev"]["os_image_hash"], dev_hash);
    assert_eq!(
        written["flavors"]["dev"]["image_dir"],
        "out/dev/dstack-dev-0.6.3"
    );
    assert_eq!(written["flavors"]["dev"]["archive"], dev_archive);
    assert_eq!(
        written["flavors"]["dev"]["archive_sha256"],
        archive::sha256_file_streaming(&output_dir.join(&dev_archive)).unwrap()
    );
    assert_eq!(written["flavors"]["prod"]["os_image_hash"], prod_hash);
    assert_eq!(
        written["flavors"]["prod"]["image_dir"],
        "out/prod/dstack-0.6.3"
    );

    // `verify_image_dir` validates the built output directory against `vm-image-manifest.json`.
    let verified = archive::verify_image_dir(
        &output_dir.join("out/dev/dstack-dev-0.6.3"),
        Some(&output_dir.join(MANIFEST_NAME)),
        Some(Flavor::Dev),
    )
    .unwrap();
    assert_eq!(verified.os_image_hash, dev_hash);

    // Archive reproducibility mismatch check rejects differing archive SHA-256s.
    let mut fake_a = BTreeMap::new();
    let mut fake_b = BTreeMap::new();
    fake_a.insert(
        "dev".to_owned(),
        FlavorArtifact {
            os_image_hash: dev_hash.clone(),
            image_dir: "out/dev/dstack-dev-0.6.3".to_owned(),
            archive: Some(dev_archive.clone()),
            archive_sha256: Some("a".repeat(64)),
            files: Vec::new(),
        },
    );
    fake_b.insert(
        "dev".to_owned(),
        FlavorArtifact {
            os_image_hash: dev_hash,
            image_dir: "out/dev/dstack-dev-0.6.3".to_owned(),
            archive: Some(dev_archive),
            archive_sha256: Some("b".repeat(64)),
            files: Vec::new(),
        },
    );
    let err = compare_packed_archives(&fake_a, &fake_b)
        .expect_err("differing archive sha256 across runs must fail");
    assert!(
        err.to_string().contains("archive SHA-256 mismatch"),
        "{err}"
    );
}

#[test]
fn repro_check_two_run_orchestration_fails_and_names_flavor_and_differing_file() {
    let temp = tempfile::tempdir().unwrap();
    let work_dir = temp.path().join("work");
    let dstack_src = work_dir.join("dstack-outbe");
    let output_dir = temp.path().join("output");
    fs::create_dir_all(&output_dir).unwrap();
    let overlay_commit = init_fake_overlay_repo(&dstack_src);
    let dev_hash = "1".repeat(64);
    let prod_hash = "2".repeat(64);

    let mut call_idx = 0usize;
    let err = execute_repro_builds(
        &BuildDirs {
            work_dir: &work_dir,
            dstack_src: &dstack_src,
            output_dir: &output_dir,
        },
        &BuildIdentity {
            source_commit: &"a".repeat(40),
            dstack_rev: DSTACK_COMMIT,
            overlay_commit: &overlay_commit,
            dstack_version: "0.6.3",
            flavors: &[Flavor::Dev, Flavor::Prod],
        },
        true,
        |_run_src, run_out| {
            call_idx += 1;
            let dev_bytes = if call_idx == 1 {
                b"dev-img-a".as_slice()
            } else {
                b"dev-img-b".as_slice()
            };
            write_fake_flavor_output(run_out, Flavor::Dev, "0.6.3", &dev_hash, dev_bytes);
            write_fake_flavor_output(run_out, Flavor::Prod, "0.6.3", &prod_hash, b"prod-img");
            Ok(())
        },
    )
    .unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("flavor 'dev'"), "{msg}");
    assert!(msg.contains("rootfs.img"), "{msg}");
    assert!(!msg.contains("flavor 'prod'"), "{msg}");
    assert!(!output_dir.join("out").exists());
}
