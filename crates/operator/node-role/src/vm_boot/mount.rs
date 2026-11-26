//! Mount option check of the VM data disk against `/proc/self/mountinfo`.

use std::{collections::BTreeSet, path::Path};

use eyre::{ensure, Result};

const REQUIRED_MOUNT_OPTIONS: &[&str] = &["noexec", "nosuid", "nodev"];

pub(super) fn verify_mount(root: &Path, mountinfo: &str) -> Result<()> {
    let target = root.to_string_lossy();
    let target = target.trim_end_matches('/');
    let mut matched_options: Option<&str> = None;

    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let mount_point = decode_mountinfo_path(fields[4]);
        if mount_point.trim_end_matches('/') == target {
            matched_options = Some(fields[5]);
        }
    }

    let raw_options =
        matched_options.ok_or_else(|| eyre::eyre!("{} is not a mount point", root.display()))?;
    let present: BTreeSet<&str> = raw_options.split(',').collect();
    let missing: Vec<&str> = REQUIRED_MOUNT_OPTIONS
        .iter()
        .copied()
        .filter(|option| !present.contains(option))
        .collect();
    ensure!(
        missing.is_empty(),
        "{} mount is missing required options: {} (got: {raw_options})",
        root.display(),
        missing.join(", "),
    );
    Ok(())
}

fn decode_mountinfo_path(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let octal = &encoded[index + 1..index + 4];
            if let Ok(value) = u8::from_str_radix(octal, 8) {
                decoded.push(value);
                index += 4;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
