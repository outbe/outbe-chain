use std::process::{Command, Stdio};

use walkdir::WalkDir;

use super::{
    tar_extract::{extract_tar_stream_with_limits, MAX_TAR_HEADER_METADATA_BYTES},
    *,
};
use crate::bundle::{self, BundleInput};

fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn build_fake_operator_bundle_with_script(dir: &Path, chain_id: u64, verify_body: &str) -> PathBuf {
    let src = dir.join("bundle-src");
    let genesis = src.join("genesis.json");
    write_mode(
        &genesis,
        format!(r#"{{"config":{{"chainId":{chain_id}}}}}"#).as_bytes(),
        0o644,
    );
    let crs = src.join("bn254_g1.dat");
    write_mode(&crs, b"fake-crs-bytes", 0o644);
    let keygen = src.join("outbe-keygen");
    let script = format!(
        r#"#!/bin/sh
set -eu
cmd="$1"
shift
if [ "$cmd" = "verify" ]; then
{verify_body}
fi
if [ "$cmd" = "validator" ]; then
    out=""
    while [ $# -gt 0 ]; do
        if [ "$1" = "--output-dir" ]; then
            out="$2"
            shift 2
        else
            shift 2
        fi
    done
    umask 077
    mkdir -p "$out/radicle/keys"
    printf '%s' '1111111111111111111111111111111111111111111111111111111111111111' > "$out/signing-key.hex"
    printf '%s' '2222222222222222222222222222222222222222222222222222222222222222' > "$out/evm-key.hex"
    printf '%s' '3333333333333333333333333333333333333333333333333333333333333333' > "$out/feeder-key.hex"
    printf '%s' '4444444444444444444444444444444444444444444444444444444444444444' > "$out/hyperlane-key.hex"
    printf '%s\n' '5555555555555555555555555555555555555555555555555555555555555555' > "$out/reth-p2p-secret.hex"
    printf '%s' 'fake-radicle-ssh-key' > "$out/radicle/keys/radicle"
    exit 0
fi
exit 1
"#
    );
    write_mode(&keygen, script.as_bytes(), 0o755);
    let dummy_bin = src.join("outbe-chain");
    write_mode(&dummy_bin, b"#!/bin/sh\nexit 0\n", 0o755);

    let bundle_path = dir.join("operator-bundle.tar.zst");
    bundle::create_archive(
        &bundle_path,
        vec![
            BundleInput {
                path: "genesis.json".into(),
                source: genesis,
                mode: 0o644,
            },
            BundleInput {
                path: ".bb-crs/bn254_g1.dat".into(),
                source: crs,
                mode: 0o644,
            },
            BundleInput {
                path: "outbe-keygen".into(),
                source: keygen,
                mode: 0o755,
            },
            BundleInput {
                path: "outbe-chain".into(),
                source: dummy_bin,
                mode: 0o755,
            },
        ],
    )
    .unwrap();
    bundle_path
}

fn build_fake_operator_bundle(dir: &Path, chain_id: u64) -> PathBuf {
    // Default verify script enforces 64 hex chars on signing-key.hex (rejects prose like
    // "not-a-BLS-private-key").
    build_fake_operator_bundle_with_script(
        dir,
        chain_id,
        r#"    key_path=""
    while [ $# -gt 0 ]; do
        if [ "$1" = "--key" ]; then
            key_path="$2"
            shift 2
        else
            shift 1
        fi
    done
    val="$(tr -d '\r\n' < "$key_path")"
    case "$val" in
        *[!0-9a-fA-F]*|"")
            echo "SECRET_STDOUT_LEAK_MARKER"
            echo "SECRET_STDERR_LEAK_MARKER" >&2
            exit 1
            ;;
    esac
    [ "${#val}" -eq 64 ] || exit 1
    exit 0"#,
    )
}

fn write_joining_config(path: &Path, chain_id: u64) {
    let peer_key = "a".repeat(96);
    let yaml = format!(
        r#"version: 1
network:
  chain_id: {chain_id}
  certified_rpc: "http://10.77.0.10:8545"
  advertise_ip: 10.77.0.20
  vm_ip: 10.77.0.20
  vm_prefix: 24
  vm_gateway: 10.77.0.1
  vm_dns: [1.1.1.1, 8.8.8.8]
  bootnodes:
    - "enode://abcd@10.77.0.10:30303"
  consensus_peers:
    - "{peer_key}@10.77.0.10:30400"
  use_local_defaults: true
feeder:
  public_toml: |
    [oracle]
    sources = "local"
hyperlane:
  public_url: "http://10.77.0.20:9000"
  gasless: true
  origins:
    - domain: {chain_id}
      rpc_urls: ["http://127.0.0.1:8545"]
      rpc_consensus: single
      reorg_period: 0
"#
    );
    fs::write(path, yaml).unwrap();
}

fn write_full_node_config(path: &Path, chain_id: u64) {
    let yaml = format!(
        r#"version: 1
network:
  chain_id: {chain_id}
  certified_rpc: "http://10.77.0.10:8545"
  advertise_ip: 10.77.0.21
  vm_ip: 10.77.0.21
  vm_prefix: 24
  vm_gateway: 10.77.0.1
  vm_dns: [1.1.1.1, 8.8.8.8]
  bootnodes:
    - "enode://abcd@10.77.0.10:30303"
  use_local_defaults: true
"#
    );
    fs::write(path, yaml).unwrap();
}

fn write_valid_keys_dir(keys_dir: &Path) {
    for (rel, ch) in [
        ("signing-key.hex", "1"),
        ("evm-key.hex", "2"),
        ("feeder-key.hex", "3"),
        ("hyperlane-key.hex", "4"),
        ("reth-p2p-secret.hex", "5"),
    ] {
        write_mode(&keys_dir.join(rel), ch.repeat(64).as_bytes(), 0o600);
    }
    write_mode(
        &keys_dir.join("radicle/keys/radicle"),
        b"fake-radicle-ssh-key",
        0o600,
    );
}

#[test]
fn joining_validator_stages_keys_marker_configs_and_strips_bundle_binaries() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = build_fake_operator_bundle(tmp.path(), 707);
    let cfg = tmp.path().join("operator.yaml");
    write_joining_config(&cfg, 707);

    let staged = prepare_joining_validator_tree(&bundle, &cfg, None, tmp.path()).unwrap();
    assert_eq!(staged.role, NodeRole::JoiningValidator);
    assert!(staged.key_verified);
    assert!(staged.validator_address.is_some());
    assert!(staged.hyperlane_address.is_some());

    let tree = staged.tree_path();
    assert!(fs::read_to_string(tree.join("validator.env"))
        .unwrap()
        .contains("OUTBE_NODE_ROLE=\"joining-validator\"\n"));
    assert!(fs::read_to_string(tree.join("validator.env"))
        .unwrap()
        .contains("OUTBE_HYPERLANE_FUNDING_WEI=\"1000000000000000000\"\n"));
    assert!(fs::read_to_string(tree.join("hyperlane-public.toml"))
        .unwrap()
        .contains("bind_address = \"10.77.0.20:9003\""));
    assert!(!tree.join(crate::NODE_ROLE_FILE).exists());
    // Bundle executables MUST NOT be copied onto the noexec VM data disk.
    assert!(!tree.join("outbe-keygen").exists());
    assert!(!tree.join("outbe-chain").exists());
    assert!(!tree.join("operator-manifest.json").exists());

    // Secret permissions are strictly 0600 and reth-p2p-secret.hex has no trailing newline.
    assert_eq!(
        fs::metadata(tree.join("keys/evm-key.hex"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::read_to_string(tree.join("keys/reth-p2p-secret.hex"))
            .unwrap()
            .len(),
        64
    );
}

#[test]
fn keys_from_enforces_strict_allowlist_and_rejects_extra_files_or_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = build_fake_operator_bundle(tmp.path(), 707);
    let cfg = tmp.path().join("operator.yaml");
    write_joining_config(&cfg, 707);

    // 1. Exact 6 secrets + optional radicle.pub passes.
    let valid_keys = tmp.path().join("valid-keys");
    write_valid_keys_dir(&valid_keys);
    write_mode(
        &valid_keys.join("radicle/keys/radicle.pub"),
        b"ssh-ed25519 AAAA test",
        0o644,
    );
    let staged =
        prepare_joining_validator_tree(&bundle, &cfg, Some(&valid_keys), tmp.path()).unwrap();
    assert!(staged
        .tree_path()
        .join("keys/radicle/keys/radicle.pub")
        .is_file());

    // 2. Extra unrelated file in --keys-from is refused.
    let extra_file_keys = tmp.path().join("extra-file-keys");
    write_valid_keys_dir(&extra_file_keys);
    write_mode(
        &extra_file_keys.join("unrelated-backup.env"),
        b"AWS_SECRET=oops",
        0o600,
    );
    let err = prepare_joining_validator_tree(&bundle, &cfg, Some(&extra_file_keys), tmp.path())
        .err()
        .expect("--keys-from with extra file must fail");
    assert!(
        err.to_string()
            .contains("refusing unexpected file in --keys-from: unrelated-backup.env"),
        "{err}"
    );

    // 3. Extra unrelated directory in --keys-from is refused.
    let extra_dir_keys = tmp.path().join("extra-dir-keys");
    write_valid_keys_dir(&extra_dir_keys);
    fs::create_dir_all(extra_dir_keys.join("other-wallet")).unwrap();
    let err = prepare_joining_validator_tree(&bundle, &cfg, Some(&extra_dir_keys), tmp.path())
        .err()
        .expect("--keys-from with extra dir must fail");
    assert!(
        err.to_string()
            .contains("refusing unexpected directory in --keys-from: other-wallet"),
        "{err}"
    );
}

#[test]
fn full_node_stages_zero_keys_and_rejects_keys_from_or_validator_config() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = build_fake_operator_bundle(tmp.path(), 707);
    let full_cfg = tmp.path().join("fullnode.yaml");
    write_full_node_config(&full_cfg, 707);

    let staged = prepare_full_node_tree(&bundle, &full_cfg, None, tmp.path()).unwrap();
    assert_eq!(staged.role, NodeRole::FullNode);
    assert!(staged.validator_address.is_none());
    assert!(staged.hyperlane_address.is_none());

    let tree = staged.tree_path();
    assert!(!tree.join(crate::NODE_ROLE_FILE).exists());
    assert!(tree.join("genesis.json").is_file());
    assert!(tree.join(".bb-crs/bn254_g1.dat").is_file());
    assert!(tree.join("validator.env").is_file());
    assert!(!tree.join("keys").exists());
    assert!(!tree.join("feeder-public.toml").exists());
    assert!(!tree.join("hyperlane-public.toml").exists());
    assert!(!tree.join("outbe-chain").exists());

    let env_text = fs::read_to_string(tree.join("validator.env")).unwrap();
    assert!(env_text.contains("OUTBE_NODE_ROLE=\"full-node\"\n"));
    assert!(!env_text.contains("OUTBE_VALIDATOR_ADDRESS"));
    assert!(!env_text.contains("OUTBE_CONSENSUS_PEERS"));

    // Passing --keys-from for full-node must be refused.
    let err = prepare_full_node_tree(&bundle, &full_cfg, Some(tmp.path()), tmp.path())
        .err()
        .expect("full-node must refuse --keys-from");
    assert!(err.to_string().contains("zero keys"), "{err}");

    // Passing an OperatorConfig with feeder/hyperlane/consensus_peers must be refused.
    let op_cfg = tmp.path().join("operator.yaml");
    write_joining_config(&op_cfg, 707);
    let err = prepare_full_node_tree(&bundle, &op_cfg, None, tmp.path())
        .err()
        .expect("full-node must reject validator sections");
    assert!(
        format!("{err:#}").contains("unknown field"),
        "expected deny_unknown_fields error, got: {err:#}"
    );
}

#[test]
fn founder_requires_verified_keygen_or_explicit_skip_and_rejects_bad_bls_key() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = build_fake_operator_bundle(tmp.path(), 707);
    let cfg = tmp.path().join("operator.yaml");
    write_joining_config(&cfg, 707);
    let joining = prepare_joining_validator_tree(&bundle, &cfg, None, tmp.path()).unwrap();

    // 1. Standalone founder dir without sibling bundle fails closed under Auto.
    let standalone = tmp.path().join("standalone-founder");
    copy_dir_plain(joining.tree_path(), &standalone);
    rewrite_env_role(&standalone, "founder");
    let err = prepare_founder_tree(&standalone, tmp.path(), FounderKeyVerify::Auto)
        .err()
        .expect("founder without bundle must refuse unless --skip-key-verify");
    assert!(
        err.to_string()
            .contains("no verified operator bundle found"),
        "{err}"
    );

    // 2. Explicit Skip succeeds and reports key_verified == false.
    let skipped = prepare_founder_tree(&standalone, tmp.path(), FounderKeyVerify::Skip).unwrap();
    assert!(!skipped.key_verified);

    // 3. Explicit Bundle runs real outbe-keygen verify; corrupt signing-key.hex fails without
    // leaking keygen stdout/stderr.
    let staged_ok =
        prepare_founder_tree(&standalone, tmp.path(), FounderKeyVerify::Bundle(&bundle)).unwrap();
    assert!(staged_ok.key_verified);

    write_mode(
        &standalone.join("keys/signing-key.hex"),
        b"not-a-BLS-private-key",
        0o600,
    );
    let err = prepare_founder_tree(&standalone, tmp.path(), FounderKeyVerify::Bundle(&bundle))
        .err()
        .expect("corrupt founder signing-key.hex must fail verification");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("signing-key.hex failed outbe-keygen verify"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("SECRET_STDOUT_LEAK_MARKER")
            && !rendered.contains("SECRET_STDERR_LEAK_MARKER"),
        "verify failure must never leak stdout/stderr: {rendered}"
    );

    // 4. Auto-discovery finds <artifacts>/joining/outbe-validator-operator-bundle.tar.zst.
    let artifacts = tmp.path().join("net-artifacts");
    let vm_founder = artifacts.join("vm/validator-0");
    copy_dir_plain(joining.tree_path(), &vm_founder);
    rewrite_env_role(&vm_founder, "founder");
    fs::create_dir_all(artifacts.join("joining")).unwrap();
    fs::copy(
        &bundle,
        artifacts.join("joining/outbe-validator-operator-bundle.tar.zst"),
    )
    .unwrap();
    let auto_staged =
        prepare_founder_tree(&vm_founder, tmp.path(), FounderKeyVerify::Auto).unwrap();
    assert!(auto_staged.key_verified);
    assert!(auto_staged
        .keygen_source
        .as_deref()
        .unwrap_or_default()
        .starts_with("artifacts:"));
}

#[test]
fn founder_tar_rejects_traversal_and_extracts_wrapped_valid_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = build_fake_operator_bundle(tmp.path(), 707);
    let cfg = tmp.path().join("operator.yaml");
    write_joining_config(&cfg, 707);
    let joining = prepare_joining_validator_tree(&bundle, &cfg, None, tmp.path()).unwrap();
    let founder_dir = tmp.path().join("validator-0");
    copy_dir_plain(joining.tree_path(), &founder_dir);
    rewrite_env_role(&founder_dir, "founder");

    // Pack into a .tar.zst wrapped in `validator-0/`.
    let tar_zst = tmp.path().join("validator-0.tar.zst");
    {
        let file = File::create(&tar_zst).unwrap();
        let mut enc = zstd::Encoder::new(file, 3).unwrap();
        {
            let mut builder = tar::Builder::new(&mut enc);
            builder.append_dir_all("validator-0", &founder_dir).unwrap();
            builder.finish().unwrap();
        }
        enc.finish().unwrap();
    }
    let staged =
        prepare_founder_tree(&tar_zst, tmp.path(), FounderKeyVerify::Bundle(&bundle)).unwrap();
    assert_eq!(staged.role, NodeRole::Founder);
    assert!(staged.key_verified);
    assert!(staged.tree_path().join("genesis.json").is_file());
    assert!(!staged.tree_path().join(crate::NODE_ROLE_FILE).exists());

    // Tar with an executable file is rejected.
    let bad_tar = tmp.path().join("bad-exec.tar");
    {
        let file = File::create(&bad_tar).unwrap();
        let mut builder = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o755);
        header.set_size(4);
        header.set_cksum();
        builder
            .append_data(&mut header, "evil.sh", &b"#!/\n"[..])
            .unwrap();
        builder.finish().unwrap();
    }
    let err = prepare_founder_tree(&bad_tar, tmp.path(), FounderKeyVerify::Skip)
        .err()
        .expect("tar with executable must fail");
    assert!(err.to_string().contains("refusing executable"), "{err}");
}

#[test]
fn founder_tar_enforces_decompressed_stream_and_header_metadata_budgets() {
    let tmp = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);

    // 1. Directory entry with non-zero declared payload size (e.g. a 600 MiB dir bomb) is
    // rejected immediately without allocating or skipping the payload.
    let dir_bomb_tar = tmp.path().join("dir-bomb.tar");
    {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_size(600 * 1024 * 1024);
        header.set_path("bomb-dir").unwrap();
        header.set_cksum();
        let mut f = File::create(&dir_bomb_tar).unwrap();
        f.write_all(header.as_bytes()).unwrap();
        f.write_all(&[0u8; 1024]).unwrap();
    }
    let err = extract_founder_tar(&dir_bomb_tar, &tmp.path().join("out-dir"), &cancel)
        .expect_err("directory with non-zero payload must be rejected");
    assert!(
        err.to_string()
            .contains("directory entry with non-zero payload size"),
        "{err}"
    );

    // 2. Oversized PAX extended header (> 1 MiB) is rejected BEFORE tar allocates it.
    let pax_bomb_zst = tmp.path().join("pax-bomb.tar.zst");
    {
        let file = File::create(&pax_bomb_zst).unwrap();
        let mut enc = zstd::Encoder::new(file, 3).unwrap();
        let pax_size: u64 = 2 * 1024 * 1024;
        let mut pax_header = tar::Header::new_ustar();
        pax_header.set_entry_type(tar::EntryType::XHeader);
        pax_header.set_mode(0o644);
        pax_header.set_size(pax_size);
        pax_header.set_path("PaxHeaders.0/huge").unwrap();
        pax_header.set_cksum();
        enc.write_all(pax_header.as_bytes()).unwrap();
        let chunk = vec![b'a'; 64 * 1024];
        let mut rem = pax_size as usize;
        while rem > 0 {
            let n = rem.min(chunk.len());
            enc.write_all(&chunk[..n]).unwrap();
            rem -= n;
        }
        enc.finish().unwrap();
    }
    let err = extract_founder_tar(&pax_bomb_zst, &tmp.path().join("out-pax"), &cancel)
        .expect_err("oversized PAX header must fail before allocation");
    assert!(
        format!("{err:#}").contains("entry header/metadata exceeds maximum"),
        "{err:#}"
    );

    // 3. Total decompressed stream cap counts headers + padding + payloads (even small files
    // whose headers alone push total decompressed bytes past the budget).
    let stream_tar = tmp.path().join("stream-over.tar");
    {
        let file = File::create(&stream_tar).unwrap();
        let mut builder = tar::Builder::new(file);
        for i in 0..8 {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Regular);
            h.set_mode(0o644);
            h.set_size(1);
            h.set_cksum();
            builder
                .append_data(&mut h, format!("f{i}.txt"), &b"x"[..])
                .unwrap();
        }
        builder.finish().unwrap();
    }
    // 8 entries * 1024 bytes (header + padded block) + 1024 EOF > 4096 bytes, even though
    // file payload bytes total only 8 bytes!
    let f = File::open(&stream_tar).unwrap();
    let err = extract_tar_stream_with_limits(
        f,
        &tmp.path().join("out-stream"),
        4096,
        MAX_TAR_HEADER_METADATA_BYTES,
        &cancel,
    )
    .expect_err("decompressed tar headers + padding exceeding total cap must fail");
    assert!(
        format!("{err:#}").contains("decompressed stream exceeds maximum"),
        "{err:#}"
    );
}

#[test]
fn cleanup_stale_staging_dirs_removes_only_matching_non_symlink_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let stale_a = tmp.path().join(".outbe-prepare-123");
    let stale_b = tmp.path().join(".outbe-disk-founder-456");
    let stale_c = tmp.path().join(".outbe-vm-disk-789");
    let keep_dir = tmp.path().join("unrelated-dir");
    let keep_file = tmp.path().join(".outbe-disk-regular-file");
    let outside_dir = tmp.path().join("outside-target");
    let symlink_dir = tmp.path().join(".outbe-disk-symlink");

    for d in [&stale_a, &stale_b, &stale_c, &keep_dir, &outside_dir] {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(&keep_file, b"keep").unwrap();
    std::os::unix::fs::symlink(&outside_dir, &symlink_dir).unwrap();

    let live_locked = create_locked_staging_dir(tmp.path(), ".outbe-disk-live-").unwrap();
    let live_path = live_locked.path().to_path_buf();
    fs::write(live_path.join("in-flight.hex"), b"live").unwrap();

    let removed = cleanup_stale_staging_dirs(tmp.path()).unwrap();
    assert_eq!(removed.len(), 3);
    assert!(!stale_a.exists());
    assert!(!stale_b.exists());
    assert!(!stale_c.exists());
    assert!(
        live_path.is_dir() && live_path.join("in-flight.hex").is_file(),
        "cleanup_stale_staging_dirs must NEVER delete a live locked staging dir"
    );
    assert!(keep_dir.is_dir());
    assert!(keep_file.is_file());
    assert!(outside_dir.is_dir());
    assert!(fs::symlink_metadata(&symlink_dir).is_ok());
    drop(live_locked);
}

#[test]
fn founder_from_reader_accepts_plain_zstd_and_gzip_tar_and_rejects_attacks() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = build_fake_operator_bundle(tmp.path(), 707);
    let cfg = tmp.path().join("operator.yaml");
    write_joining_config(&cfg, 707);
    let joining = prepare_joining_validator_tree(&bundle, &cfg, None, tmp.path()).unwrap();
    let founder_dir = tmp.path().join("founder-src");
    copy_dir_plain(joining.tree_path(), &founder_dir);
    rewrite_env_role(&founder_dir, "founder");

    // Build plain tar in memory.
    let mut plain_tar = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut plain_tar);
        builder.append_dir_all(".", &founder_dir).unwrap();
        builder.finish().unwrap();
    }

    // 1. Auto key-verify is refused on stdin streams.
    let err = prepare_founder_tree_from_reader(
        io::Cursor::new(plain_tar.clone()),
        tmp.path(),
        FounderKeyVerify::Auto,
    )
    .err()
    .expect("stdin tar with Auto key-verify must fail closed");
    assert!(
        err.to_string()
            .contains("cannot auto-discover operator bundle"),
        "{err}"
    );

    // 2. Plain tar succeeds with verified operator bundle.
    let staged_plain = prepare_founder_tree_from_reader(
        io::Cursor::new(plain_tar.clone()),
        tmp.path(),
        FounderKeyVerify::Bundle(&bundle),
    )
    .unwrap();
    assert_eq!(staged_plain.role, NodeRole::Founder);
    assert!(staged_plain.key_verified);
    assert!(staged_plain.tree_path().join("genesis.json").is_file());
    drop(staged_plain);

    // 3. Zstd-compressed tar stream succeeds.
    let zstd_tar = zstd::stream::encode_all(io::Cursor::new(&plain_tar), 1).unwrap();
    let staged_zstd = prepare_founder_tree_from_reader(
        io::Cursor::new(zstd_tar),
        tmp.path(),
        FounderKeyVerify::Bundle(&bundle),
    )
    .unwrap();
    assert!(staged_zstd.key_verified);
    drop(staged_zstd);

    // 4. Gzip-compressed tar stream succeeds.
    let mut gzip_child = Command::new("gzip")
        .arg("-c")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    gzip_child
        .stdin
        .take()
        .unwrap()
        .write_all(&plain_tar)
        .unwrap();
    let gzip_out = gzip_child.wait_with_output().unwrap();
    assert!(gzip_out.status.success());
    let staged_gz = prepare_founder_tree_from_reader(
        io::Cursor::new(gzip_out.stdout),
        tmp.path(),
        FounderKeyVerify::Bundle(&bundle),
    )
    .unwrap();
    assert!(staged_gz.key_verified);
    drop(staged_gz);

    // 5. Reject path traversal (`..`), symlinks, and executable files in streamed tar.
    let mut evil_traversal = Vec::new();
    {
        let mut h = tar::Header::new_gnu();
        h.set_size(4);
        h.set_mode(0o600);
        h.set_entry_type(tar::EntryType::Regular);
        h.as_gnu_mut().unwrap().name[..10].copy_from_slice(b"../escape\0");
        h.set_cksum();
        evil_traversal.extend_from_slice(h.as_bytes());
        let mut block = [0u8; 512];
        block[..4].copy_from_slice(b"evil");
        evil_traversal.extend_from_slice(&block);
        evil_traversal.extend_from_slice(&[0u8; 1024]);
    }
    let err = prepare_founder_tree_from_reader(
        io::Cursor::new(evil_traversal),
        tmp.path(),
        FounderKeyVerify::Skip,
    )
    .err()
    .expect("traversal tar stream must fail");
    assert!(err.to_string().contains("unsafe path"), "{err}");

    let mut evil_symlink = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut evil_symlink);
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        builder
            .append_link(&mut h, "keys/link", "/etc/passwd")
            .unwrap();
        builder.finish().unwrap();
    }
    let err = prepare_founder_tree_from_reader(
        io::Cursor::new(evil_symlink),
        tmp.path(),
        FounderKeyVerify::Skip,
    )
    .err()
    .expect("symlink tar stream must fail");
    assert!(err.to_string().contains("non-regular entry"), "{err}");

    let mut evil_exec = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut evil_exec);
        let mut h = tar::Header::new_gnu();
        h.set_size(4);
        h.set_mode(0o755);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        builder
            .append_data(&mut h, "run.sh", &b"#!/\n"[..])
            .unwrap();
        builder.finish().unwrap();
    }
    let err = prepare_founder_tree_from_reader(
        io::Cursor::new(evil_exec),
        tmp.path(),
        FounderKeyVerify::Skip,
    )
    .err()
    .expect("executable tar stream must fail");
    assert!(err.to_string().contains("executable file"), "{err}");

    // 6. Empty stream fails cleanly.
    let err = prepare_founder_tree_from_reader(
        io::Cursor::new(Vec::<u8>::new()),
        tmp.path(),
        FounderKeyVerify::Skip,
    )
    .err()
    .expect("empty tar stream must fail");
    assert!(err.to_string().contains("empty"), "{err}");

    // 7. Joining-validator tar stream passes `prepare_joining_validator_tree_from_reader`
    //    and rejects a founder tar (missing `node-role: joining-validator`).
    let mut joining_tar = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut joining_tar);
        builder.append_dir_all(".", joining.tree_path()).unwrap();
        builder.finish().unwrap();
    }
    let staged_join = prepare_joining_validator_tree_from_reader(
        io::Cursor::new(joining_tar),
        tmp.path(),
        FounderKeyVerify::Bundle(&bundle),
    )
    .unwrap();
    assert_eq!(staged_join.role, NodeRole::JoiningValidator);
    assert!(staged_join.key_verified);
    assert!(
        fs::read_to_string(staged_join.tree_path().join("validator.env"))
            .unwrap()
            .contains("OUTBE_NODE_ROLE=\"joining-validator\"\n")
    );
    drop(staged_join);

    let err = prepare_joining_validator_tree_from_reader(
        io::Cursor::new(plain_tar),
        tmp.path(),
        FounderKeyVerify::Bundle(&bundle),
    )
    .err()
    .expect("founder tar must be rejected by joining-validator reader");
    assert!(err.to_string().contains("role mismatch"), "{err}");
}

fn rewrite_env_role(tree: &Path, role: &str) {
    let env_path = tree.join("validator.env");
    let raw = fs::read_to_string(&env_path).unwrap();
    let replaced = raw.replace(
        "OUTBE_NODE_ROLE=\"joining-validator\"",
        &format!("OUTBE_NODE_ROLE=\"{role}\""),
    );
    fs::write(&env_path, replaced).unwrap();
    fs::set_permissions(&env_path, fs::Permissions::from_mode(0o640)).unwrap();
}

fn copy_dir_plain(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in WalkDir::new(src).follow_links(false) {
        let entry = entry.unwrap();
        let rel = entry.path().strip_prefix(src).unwrap();
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = dst.join(rel);
        let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
        } else {
            fs::copy(entry.path(), &target).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
        }
    }
}
