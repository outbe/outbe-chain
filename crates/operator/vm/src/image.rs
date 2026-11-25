//! Guest OS image validation, digest-qualified naming, and atomic host publication commands.

use std::{fs, path::Path};

use eyre::{bail, ensure, Context, Result};

use crate::{
    dnsmasq::{MISSING_SENTINEL, PRESENT_SENTINEL},
    shell_quote,
    vmm::remote_vmm_path,
};

#[derive(Debug, Eq, PartialEq)]
pub enum RemoteImageAction {
    Upload,
    Reuse,
}

/// Validate a local guest OS image directory and derive its digest-qualified VMM image name.
pub fn validate_image_dir(
    image_dir: &Path,
    image_name: Option<&str>,
    flavor_label: &str,
) -> Result<(String, String)> {
    ensure!(
        image_dir.is_dir(),
        "guest image directory does not exist: {}",
        image_dir.display()
    );
    let metadata_path = image_dir.join("metadata.json");
    ensure!(
        metadata_path.is_file(),
        "guest image directory is missing metadata.json: {}",
        metadata_path.display()
    );
    let digest_path = image_dir.join("digest.txt");
    ensure!(
        digest_path.is_file(),
        "guest image directory is missing digest.txt: {}",
        digest_path.display()
    );
    let digest = fs::read_to_string(&digest_path)
        .wrap_err_with(|| format!("read {}", digest_path.display()))?
        .trim()
        .to_ascii_lowercase();
    ensure!(
        digest.len() >= 16 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "digest.txt must contain at least 16 hex characters"
    );

    let name = match image_name {
        Some(explicit) => explicit.to_owned(),
        None => format!("outbe-vm-{flavor_label}-{}", &digest[..16]),
    };
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
        "invalid guest image name: {name}"
    );
    Ok((name, digest))
}

/// Decide whether to reuse or upload a guest image on a host, refusing digest mismatches.
pub fn decide_remote_image_action(
    remote_state: Option<&str>,
    local_digest: &str,
    image_name: &str,
) -> Result<RemoteImageAction> {
    match remote_state {
        None => Ok(RemoteImageAction::Upload),
        Some(remote_digest) => {
            let trimmed = remote_digest.trim().to_ascii_lowercase();
            if trimmed == local_digest {
                Ok(RemoteImageAction::Reuse)
            } else {
                bail!(
                    "refusing to replace existing guest image `{image_name}`: remote digest `{trimmed}` does not match local `{local_digest}`"
                );
            }
        }
    }
}

/// Build the host shell command that reads `<vmm_home>/image/<image_name>/digest.txt` with sentinels.
pub fn build_remote_image_check_command(vmm_home: &str, image_name: &str) -> String {
    let remote_dir_expr = remote_vmm_path(vmm_home, &format!("image/{image_name}"));
    let remote_digest_expr = remote_vmm_path(vmm_home, &format!("image/{image_name}/digest.txt"));
    format!(
        "if [ ! -e {remote_dir_expr} ]; then printf '{MISSING_SENTINEL}\\n'; else printf '{PRESENT_SENTINEL}\\n' && cat {remote_digest_expr}; fi"
    )
}

/// Build the locked host shell command that atomically publishes a staged guest image directory.
pub fn build_image_publish_command(
    vmm_home: &str,
    image_name: &str,
    stage_dir: &str,
    expected_digest: &str,
) -> String {
    let remote_dir_expr = remote_vmm_path(vmm_home, &format!("image/{image_name}"));
    let remote_digest_expr = remote_vmm_path(vmm_home, &format!("image/{image_name}/digest.txt"));
    let lock_expr = remote_vmm_path(vmm_home, &format!("image/.{image_name}.lock"));
    let stage_quoted = shell_quote(stage_dir);
    let expected_quoted = shell_quote(expected_digest);

    format!(
        "uid=$(id -u); \
         if [ -e {lock_expr} ] || [ -L {lock_expr} ]; then \
           if [ ! -f {lock_expr} ] || [ -L {lock_expr} ] || [ \"$(stat -c '%u' {lock_expr})\" != \"$uid\" ]; then \
             echo 'image lock must be a regular file owned by current uid' >&2; exit 1; \
           fi; \
         fi; \
         exec 9>>{lock_expr}; flock -x 9; \
         if [ -e {remote_dir_expr} ]; then \
           rm -rf {stage_quoted}; \
           if [ ! -f {remote_digest_expr} ]; then echo 'existing image is missing digest.txt' >&2; exit 1; fi; \
           actual=$(tr -d ' \\t\\r\\n' < {remote_digest_expr} | tr 'A-F' 'a-f'); \
           if [ \"$actual\" != {expected_quoted} ]; then echo \"existing image digest mismatch: $actual\" >&2; exit 1; fi; \
         else \
           mv -T {stage_quoted} {remote_dir_expr}; \
         fi; \
         if [ ! -f {remote_digest_expr} ]; then echo 'published image is missing digest.txt' >&2; exit 1; fi; \
         final=$(tr -d ' \\t\\r\\n' < {remote_digest_expr} | tr 'A-F' 'a-f'); \
         if [ \"$final\" != {expected_quoted} ]; then echo \"published image digest mismatch: $final\" >&2; exit 1; fi"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_publish_command_handles_normal_and_competitor_race() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let vmm_home = dir.path().to_str().unwrap();
        let image_root = dir.path().join("image");
        fs::create_dir_all(&image_root).unwrap();
        let image_name = "outbe-vm-dev-0123456789abcdef";
        let digest = "0123456789abcdef".repeat(4);

        // 1) Normal publication via `mv -T` + post-publish digest verification.
        let stage1 = image_root.join(".stage1");
        fs::create_dir_all(&stage1).unwrap();
        fs::write(stage1.join("digest.txt"), format!("{digest}\n")).unwrap();
        fs::write(stage1.join("metadata.json"), "{}").unwrap();

        let cmd1 =
            build_image_publish_command(vmm_home, image_name, stage1.to_str().unwrap(), &digest);
        let out1 = Command::new("sh").arg("-c").arg(&cmd1).output().unwrap();
        assert!(
            out1.status.success(),
            "normal publish failed: {}",
            String::from_utf8_lossy(&out1.stderr)
        );
        let published = image_root.join(image_name);
        assert!(published.join("digest.txt").is_file());
        assert!(!stage1.exists());

        // 2) Competitor published first with SAME digest -> cleans stage dir, no nesting, succeeds.
        let stage2 = image_root.join(".stage2");
        fs::create_dir_all(&stage2).unwrap();
        fs::write(stage2.join("digest.txt"), format!("{digest}\n")).unwrap();
        let cmd2 =
            build_image_publish_command(vmm_home, image_name, stage2.to_str().unwrap(), &digest);
        let out2 = Command::new("sh").arg("-c").arg(&cmd2).output().unwrap();
        assert!(
            out2.status.success(),
            "same-digest competitor race should reuse cleanly: {}",
            String::from_utf8_lossy(&out2.stderr)
        );
        assert!(!stage2.exists());
        assert!(
            !published.join(".stage2").exists(),
            "must never nest into target dir"
        );

        // 3) Competitor published first with DIFFERENT digest -> cleans stage dir, refuses, keeps target.
        let other_digest = "f".repeat(64);
        let stage3 = image_root.join(".stage3");
        fs::create_dir_all(&stage3).unwrap();
        fs::write(stage3.join("digest.txt"), format!("{other_digest}\n")).unwrap();
        let cmd3 = build_image_publish_command(
            vmm_home,
            image_name,
            stage3.to_str().unwrap(),
            &other_digest,
        );
        let out3 = Command::new("sh").arg("-c").arg(&cmd3).output().unwrap();
        assert!(
            !out3.status.success(),
            "different-digest competitor must fail"
        );
        assert!(!stage3.exists());
        assert_eq!(
            fs::read_to_string(published.join("digest.txt"))
                .unwrap()
                .trim(),
            digest
        );
    }

    #[test]
    fn digest_qualified_image_name_reuses_match_and_refuses_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("dstack-dev-0.6.3");
        fs::create_dir_all(&image).unwrap();
        assert!(validate_image_dir(&image, None, "dev").is_err());

        fs::write(image.join("metadata.json"), "{}").unwrap();
        let digest = "0123456789abcdef".repeat(4);
        fs::write(image.join("digest.txt"), format!("{digest}\n")).unwrap();

        let (name, parsed_digest) = validate_image_dir(&image, None, "dev").unwrap();
        assert_eq!(name, "outbe-vm-dev-0123456789abcdef");
        assert_eq!(parsed_digest, digest);

        assert_eq!(
            decide_remote_image_action(None, &digest, &name).unwrap(),
            RemoteImageAction::Upload
        );
        assert_eq!(
            decide_remote_image_action(Some(&digest), &digest, &name).unwrap(),
            RemoteImageAction::Reuse
        );
        let err = decide_remote_image_action(Some(&"f".repeat(64)), &digest, &name).unwrap_err();
        assert!(err.to_string().contains("refusing to replace"), "{err}");
    }
}
