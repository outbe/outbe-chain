//! Outbe rootfs overlay staged into the dstack mkosi skeletons, including the dev-only
//! diagnostics.

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use eyre::{ensure, Result};
use outbe_operator_systemd::{vm_console_dropins, vm_dev_dropins, vm_units};

use crate::{OUTBE_GID, OUTBE_UID, PINNED_RUNTIME_LIBS, VM_BINARIES};

/// Stage the Outbe rootfs overlay into `dstack_root/os/mkosi/mkosi.skeleton`
/// (shared by `dev` and `prod`) and `mkosi.profiles/dev/mkosi.skeleton` (`dev` only).
pub fn stage_overlay(
    dstack_root: &Path,
    software_root: &Path,
    runtime_libs_dir: &Path,
) -> Result<()> {
    let common = dstack_root.join("os/mkosi/mkosi.skeleton");
    let dev_only = dstack_root.join("os/mkosi/mkosi.profiles/dev/mkosi.skeleton");
    fs::create_dir_all(&common)?;
    fs::create_dir_all(&dev_only)?;

    // 1. Binaries from verified software bundle -> /usr/local/bin (no CRS in image).
    let bin_dst = common.join("usr/local/bin");
    fs::create_dir_all(&bin_dst)?;
    for binary in VM_BINARIES {
        let src = software_root.join("bin").join(binary);
        ensure!(
            src.is_file(),
            "software bundle is missing required VM binary: {}",
            src.display()
        );
        let dst = bin_dst.join(binary);
        fs::copy(&src, &dst)?;
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o755))?;
    }

    // 2. Pinned LLVM 18 runtime libraries -> /usr/local/lib/outbe/lib + ld.so.conf.d.
    let lib_dst = common.join("usr/local/lib/outbe/lib");
    fs::create_dir_all(&lib_dst)?;
    for pinned in PINNED_RUNTIME_LIBS {
        let src = runtime_libs_dir.join(pinned.name);
        let dst = lib_dst.join(pinned.name);
        fs::copy(&src, &dst)?;
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o644))?;
    }
    write_overlay_file(
        &common.join("etc/ld.so.conf.d/outbe.conf"),
        "/usr/local/lib/outbe/lib\n",
        0o644,
    )?;

    // 3. System account `outbe` with fixed UID/GID and home `/opt/outbe-chain`, plus tmpfiles
    //    entry ensuring `/run/systemd/network` (0755) exists before `outbe-vm-boot.service` starts,
    //    and static-network base config replacing dstack's DHCP `20-wired.network`.
    write_overlay_file(
        &common.join("usr/lib/sysusers.d/outbe.conf"),
        &render_sysusers_conf(),
        0o644,
    )?;
    write_overlay_file(
        &common.join("usr/lib/tmpfiles.d/outbe-vm-network.conf"),
        "d /run/systemd/network 0755 root root -\n",
        0o644,
    )?;
    write_overlay_file(
        &common.join("etc/systemd/network/20-wired.network"),
        &render_wired_network_no_dhcp(),
        0o644,
    )?;

    // 4. Mountpoint `/opt/outbe-chain` in the read-only rootfs.
    write_overlay_file(&common.join("opt/outbe-chain/.keep"), "", 0o644)?;

    // 5. Systemd units from `outbe_operator_systemd::vm_units()` + data bind-mount unit.
    let systemd_dir = common.join("usr/lib/systemd/system");
    fs::create_dir_all(&systemd_dir)?;
    for unit in vm_units() {
        write_overlay_file(&systemd_dir.join(unit.name), &unit.content, 0o644)?;
    }
    // 5a. Console drop-ins for both flavors: each service prints to the serial console one
    //     time. journald does not forward its entries to the console: that would print each
    //     line a second time.
    for dropin in vm_console_dropins() {
        write_overlay_file(&systemd_dir.join(dropin.name), &dropin.content, 0o644)?;
    }
    write_overlay_file(
        &systemd_dir.join("outbe-data-mount.service"),
        &render_data_mount_unit(),
        0o644,
    )?;

    // 6. Presets with `dstack` in their filenames so `mkosi.postinst` preserves them.
    //    `75-dstack-outbe-network.preset` sorts before `80-dstack.preset` so first-match-wins
    //    disables `systemd-networkd-wait-online.service`, and the `/etc/systemd/system` mask
    //    symlink overrides Debian's pre-existing `network-online.target.wants` symlink so
    //    `dstack-prepare.service` never blocks 120s waiting for DHCP before mounting the
    //    persistent data disk.
    write_overlay_file(
        &common.join("usr/lib/systemd/system-preset/75-dstack-outbe-network.preset"),
        "disable systemd-networkd-wait-online.service\n",
        0o644,
    )?;
    write_overlay_symlink(
        &common.join("etc/systemd/system/systemd-networkd-wait-online.service"),
        Path::new("/dev/null"),
    )?;
    write_overlay_file(
        &common.join("usr/lib/systemd/system-preset/85-dstack-outbe.preset"),
        "enable outbe-data-mount.service\n",
        0o644,
    )?;

    // 7. Dev-only overlay: SSH, service-IP metrics bind, and periodic diagnostic snapshots.
    //    Everything here lives strictly under `mkosi.profiles/dev/mkosi.skeleton`, so `prod`
    //    never receives any of it.
    //    SSH config is named `00-outbe-dev.conf` so OpenSSH first-match-wins overrides dstack's.
    write_overlay_file(
        &dev_only.join("etc/ssh/sshd_config.d/00-outbe-dev.conf"),
        &render_dev_sshd_conf(),
        0o644,
    )?;
    write_overlay_file(
        &dev_only.join("usr/lib/systemd/system/ssh.service.d/20-outbe-dev.conf"),
        "[Unit]\nAfter=outbe-data-mount.service\nWants=outbe-data-mount.service\n",
        0o644,
    )?;

    // 7b. Unit drop-ins for the node units: `${OUTBE_VM_IP}:9101` metrics binding.
    let dev_systemd_dir = dev_only.join("usr/lib/systemd/system");
    for dropin in vm_dev_dropins() {
        write_overlay_file(&dev_systemd_dir.join(dropin.name), &dropin.content, 0o644)?;
    }

    // 7c. Periodic 60s diagnostic snapshot timer, service, and script (`OUTBE-DIAG <unix_ts>`).
    write_overlay_file(
        &dev_only.join("usr/local/bin/outbe-vm-diag.sh"),
        &render_dev_diag_script(),
        0o755,
    )?;
    write_overlay_file(
        &dev_systemd_dir.join("outbe-vm-diag.service"),
        &render_dev_diag_service(),
        0o644,
    )?;
    write_overlay_file(
        &dev_systemd_dir.join("outbe-vm-diag.timer"),
        &render_dev_diag_timer(),
        0o644,
    )?;

    write_overlay_file(
        &dev_only.join("usr/lib/systemd/system-preset/85-dstack-outbe-dev.preset"),
        "enable ssh.service\nenable outbe-vm-diag.timer\n",
        0o644,
    )?;

    Ok(())
}

fn render_sysusers_conf() -> String {
    format!(
        "# Fixed UID/GID for the Outbe validator service account.\n\
         g outbe {OUTBE_GID} -\n\
         u outbe {OUTBE_UID}:{OUTBE_GID} \"Outbe validator\" /opt/outbe-chain /usr/sbin/nologin\n"
    )
}

fn render_wired_network_no_dhcp() -> String {
    concat!(
        "# Managed by Outbe VM image overlay (static IPv4 configured by outbe-vm-boot).\n",
        "[Match]\n",
        "Name=en* eth*\n",
        "\n",
        "[Link]\n",
        "RequiredForOnline=no\n",
        "ActivationPolicy=up\n",
        "\n",
        "[Network]\n",
        "DHCP=no\n",
        "IPv6AcceptRA=no\n",
        "LinkLocalAddressing=no\n",
        "ConfigureWithoutCarrier=yes\n",
    )
    .to_owned()
}

fn render_data_mount_unit() -> String {
    concat!(
        "[Unit]\n",
        "Description=Bind-mount Outbe persistent data directory with noexec,nosuid,nodev\n",
        "Requires=dstack-prepare.service\n",
        "After=dstack-prepare.service\n",
        "Before=outbe-vm-boot.service\n",
        "PartOf=outbe-validator.target\n",
        "\n",
        "[Service]\n",
        "Type=oneshot\n",
        "RemainAfterExit=yes\n",
        "ExecStart=/usr/bin/mount --bind /dstack/persistent/outbe-chain /opt/outbe-chain\n",
        "ExecStart=/usr/bin/mount -o remount,bind,noexec,nosuid,nodev /opt/outbe-chain\n",
        "ExecStop=/usr/bin/umount /opt/outbe-chain\n",
        "StandardOutput=journal+console\n",
        "StandardError=journal+console\n",
        "\n",
        "[Install]\n",
        "WantedBy=outbe-validator.target\n",
    )
    .to_owned()
}

fn render_dev_sshd_conf() -> String {
    concat!(
        "# Outbe dev VM SSH access using keys on the unmeasured data disk.\n",
        "PubkeyAuthentication yes\n",
        "PermitRootLogin prohibit-password\n",
        "AuthorizedKeysFile /opt/outbe-chain/dev/authorized_keys\n",
    )
    .to_owned()
}

fn render_dev_diag_service() -> String {
    concat!(
        "[Unit]\n",
        "Description=Outbe dev VM diagnostic snapshot\n",
        "After=local-fs.target\n",
        "\n",
        "[Service]\n",
        "Type=oneshot\n",
        "EnvironmentFile=-/opt/outbe-chain/validator.env\n",
        "ExecStart=/usr/local/bin/outbe-vm-diag.sh\n",
        "StandardOutput=journal+console\n",
        "StandardError=journal+console\n",
        "TimeoutStartSec=20\n",
    )
    .to_owned()
}

fn render_dev_diag_timer() -> String {
    concat!(
        "[Unit]\n",
        "Description=Outbe dev VM periodic diagnostics timer\n",
        "\n",
        "[Timer]\n",
        "OnBootSec=60s\n",
        "OnUnitActiveSec=60s\n",
        "AccuracySec=5s\n",
        "Unit=outbe-vm-diag.service\n",
        "\n",
        "[Install]\n",
        "WantedBy=timers.target\n",
    )
    .to_owned()
}

fn render_dev_diag_script() -> String {
    concat!(
        "#!/bin/bash\n",
        "# Outbe dev VM periodic diagnostic snapshot (dev flavor only).\n",
        "set -u\n",
        "\n",
        "ts=\"$(date +%s)\"\n",
        "tag=\"OUTBE-DIAG ${ts}\"\n",
        "\n",
        "emit() {\n",
        "    printf '%s %s\\n' \"$tag\" \"$*\"\n",
        "}\n",
        "\n",
        "emit_lines() {\n",
        "    local prefix=\"$1\"\n",
        "    local line\n",
        "    while IFS= read -r line; do\n",
        "        [ -n \"$line\" ] || continue\n",
        "        printf '%s %s %s\\n' \"$tag\" \"$prefix\" \"$line\"\n",
        "    done\n",
        "}\n",
        "\n",
        "awk_cmd=()\n",
        "if command -v awk >/dev/null 2>&1; then\n",
        "    awk_cmd=(awk)\n",
        "elif command -v busybox >/dev/null 2>&1; then\n",
        "    awk_cmd=(busybox awk)\n",
        "fi\n",
        "\n",
        "emit \"=== BEGIN ===\"\n",
        "\n",
        "# Emit transparent_hugepage setting once per boot (marker lives on tmpfs /run).\n",
        "boot_marker=\"${OUTBE_DIAG_BOOT_MARKER:-/run/outbe-vm-diag-boot-logged}\"\n",
        "if [ ! -e \"$boot_marker\" ]; then\n",
        "    if [ -r /sys/kernel/mm/transparent_hugepage/enabled ]; then\n",
        "        IFS= read -r thp_val < /sys/kernel/mm/transparent_hugepage/enabled || thp_val=\"unreadable\"\n",
        "        emit \"thp.enabled: ${thp_val}\"\n",
        "    else\n",
        "        emit \"thp.enabled: unavailable\"\n",
        "    fi\n",
        "    : > \"$boot_marker\" 2>/dev/null || true\n",
        "fi\n",
        "\n",
        "if [ -r /proc/loadavg ]; then\n",
        "    IFS= read -r loadavg < /proc/loadavg || loadavg=\"unreadable\"\n",
        "    emit \"loadavg: ${loadavg}\"\n",
        "fi\n",
        "\n",
        "if [ -r /proc/meminfo ]; then\n",
        "    if [ \"${#awk_cmd[@]}\" -gt 0 ]; then\n",
        "        \"${awk_cmd[@]}\" '\n",
        "            /^(MemTotal|MemFree|MemAvailable|Buffers|Cached|Active|Inactive|Active\\(anon\\)|Inactive\\(anon\\)|Active\\(file\\)|Inactive\\(file\\)|Dirty|Writeback|AnonPages|Mapped|Shmem|KReclaimable|Slab|SReclaimable|SUnreclaim|KernelStack|PageTables|Committed_AS|SwapTotal|SwapFree):/ {\n",
        "                k=$1; sub(/:$/, \"\", k)\n",
        "                out = (out ? out \" \" : \"\") k \"=\" $2 ($3 ? $3 : \"\")\n",
        "                n++\n",
        "                if (n == 8) { print out; out=\"\"; n=0 }\n",
        "            }\n",
        "            END { if (out) print out }\n",
        "        ' /proc/meminfo 2>/dev/null | emit_lines \"meminfo:\"\n",
        "    else\n",
        "        grep -E '^(MemTotal|MemFree|MemAvailable|Buffers|Cached|AnonPages|Mapped|Shmem|Slab|Committed_AS|SwapTotal|SwapFree):' /proc/meminfo 2>/dev/null \\\n",
        "            | tr -s ' ' | emit_lines \"meminfo:\"\n",
        "    fi\n",
        "fi\n",
        "\n",
        "pids=()\n",
        "if [ -n \"${OUTBE_DIAG_PID:-}\" ]; then\n",
        "    pids=(\"$OUTBE_DIAG_PID\")\n",
        "else\n",
        "    for p in /proc/[0-9]*; do\n",
        "        [ -r \"$p/comm\" ] || continue\n",
        "        IFS= read -r comm < \"$p/comm\" || continue\n",
        "        if [ \"$comm\" = \"outbe-chain\" ]; then\n",
        "            pids+=(\"${p#/proc/}\")\n",
        "        else\n",
        "            exe=\"$(readlink \"$p/exe\" 2>/dev/null || true)\"\n",
        "            case \"$exe\" in\n",
        "                */outbe-chain|*/outbe-chain\\ \\(deleted\\)) pids+=(\"${p#/proc/}\") ;;\n",
        "            esac\n",
        "        fi\n",
        "    done\n",
        "fi\n",
        "\n",
        "if [ \"${#pids[@]}\" -eq 0 ]; then\n",
        "    emit \"proc: outbe-chain not running\"\n",
        "else\n",
        "    for pid in \"${pids[@]}\"; do\n",
        "        if [ -r \"/proc/${pid}/status\" ]; then\n",
        "            status_line=\"$(grep -E '^(Name|State|VmPeak|VmSize|VmHWM|VmRSS|RssAnon|RssFile|RssShmem|VmSwap|Threads):' \"/proc/${pid}/status\" 2>/dev/null \\\n",
        "                | sed -E 's/:[[:space:]]+/=/; s/[[:space:]]+/ /g; s/ kB$/kB/' \\\n",
        "                | tr '\\n' ' ' | sed 's/ *$//' || true)\"\n",
        "            [ -n \"$status_line\" ] && emit \"proc[${pid}].status: ${status_line}\"\n",
        "        fi\n",
        "        if [ -r \"/proc/${pid}/smaps_rollup\" ]; then\n",
        "            rollup_line=\"$(grep -E '^(Rss|Pss|Pss_Anon|Pss_File|Pss_Shmem|Shared_Clean|Shared_Dirty|Private_Clean|Private_Dirty|Anonymous|AnonHugePages|Swap|SwapPss):' \"/proc/${pid}/smaps_rollup\" 2>/dev/null \\\n",
        "                | sed -E 's/:[[:space:]]+/=/; s/[[:space:]]+/ /g; s/ kB$/kB/' \\\n",
        "                | tr '\\n' ' ' | sed 's/ *$//' || true)\"\n",
        "            [ -n \"$rollup_line\" ] && emit \"proc[${pid}].smaps_rollup: ${rollup_line}\"\n",
        "        fi\n",
        "        if [ -r \"/proc/${pid}/smaps\" ] && [ \"${#awk_cmd[@]}\" -gt 0 ]; then\n",
        "            smaps_parsed=\"$(\"${awk_cmd[@]}\" '\n",
        "                function flush_vma() {\n",
        "                    if (!have) return\n",
        "                    if (inode == \"0\" && name !~ /^\\[(vdso|vvar|vvar_vclock|vsyscall)\\]$/) {\n",
        "                        if (size == 65536) { arena_cnt++; arena_rss += rss }\n",
        "                        if (rss > 0) {\n",
        "                            printf \"ANON %d range=%s Size=%dkB Rss=%dkB AnonHugePages=%dkB name=%s\\n\", rss, range, size, rss, thp, name\n",
        "                        }\n",
        "                    }\n",
        "                }\n",
        "                /^[0-9a-f]+-[0-9a-f]+ / {\n",
        "                    flush_vma()\n",
        "                    range=$1; inode=$5; name=(NF>=6 ? $6 : \"[anon]\")\n",
        "                    size=0; rss=0; thp=0; have=1\n",
        "                    next\n",
        "                }\n",
        "                /^Size:/          { size=$2+0; next }\n",
        "                /^Rss:/           { rss=$2+0; next }\n",
        "                /^AnonHugePages:/ { thp=$2+0; next }\n",
        "                END {\n",
        "                    flush_vma()\n",
        "                    printf \"ARENA64M count=%d rss_kb=%d\\n\", arena_cnt+0, arena_rss+0\n",
        "                }\n",
        "            ' \"/proc/${pid}/smaps\" 2>/dev/null || true)\"\n",
        "            if [ -n \"$smaps_parsed\" ]; then\n",
        "                arena_line=\"$(printf '%s\\n' \"$smaps_parsed\" | grep '^ARENA64M ' | sed 's/^ARENA64M //' || true)\"\n",
        "                [ -n \"$arena_line\" ] && emit \"proc[${pid}].smaps_64m_mappings: ${arena_line}\"\n",
        "                printf '%s\\n' \"$smaps_parsed\" \\\n",
        "                    | grep '^ANON ' \\\n",
        "                    | sort -rn -k2,2 | head -n 10 \\\n",
        "                    | cut -d' ' -f3- \\\n",
        "                    | emit_lines \"proc[${pid}].smaps_anon_top:\"\n",
        "            fi\n",
        "        fi\n",
        "        thread_rows=\"\"\n",
        "        for tstat in \"/proc/${pid}/task/\"*/stat; do\n",
        "            [ -r \"$tstat\" ] || continue\n",
        "            raw=\"\"\n",
        "            IFS= read -r raw < \"$tstat\" || continue\n",
        "            tid=\"${raw%% *}\"\n",
        "            rest=\"${raw#*(}\"\n",
        "            tcomm=\"${rest%)*}\"\n",
        "            after=\"${raw##*) }\"\n",
        "            set -- $after\n",
        "            tstate=\"${1:-?}\"\n",
        "            minflt=\"${8:-0}\"\n",
        "            utime=\"${12:-0}\"\n",
        "            stime=\"${13:-0}\"\n",
        "            trss=\"${22:-0}\"\n",
        "            tcpu=$(( utime + stime ))\n",
        "            thread_rows+=\"${minflt} ${tcpu} ${trss} ${tid} ${tstate} ${tcomm}\"$'\\n'\n",
        "        done\n",
        "        if [ -n \"$thread_rows\" ]; then\n",
        "            printf '%s' \"$thread_rows\" \\\n",
        "                | while read -r _mf _cpu _rss _tid st cm; do\n",
        "                      printf '%s %s\\n' \"$st\" \"$cm\"\n",
        "                  done \\\n",
        "                | sort | uniq -c | sort -rn | head -n 12 \\\n",
        "                | sed 's/^ *//' \\\n",
        "                | emit_lines \"proc[${pid}].threads_by_comm:\"\n",
        "            printf '%s' \"$thread_rows\" \\\n",
        "                | sort -rn -k1,1 -k2,2 | head -n 10 \\\n",
        "                | while read -r mf cpu rss tid st cm; do\n",
        "                      printf 'tid=%s state=%s minflt=%s cpu=%s rss_pages=%s comm=%s\\n' \"$tid\" \"$st\" \"$mf\" \"$cpu\" \"$rss\" \"$cm\"\n",
        "                  done \\\n",
        "                | emit_lines \"proc[${pid}].threads_top:\"\n",
        "        fi\n",
        "    done\n",
        "fi\n",
        "\n",
        "cport=\"${OUTBE_CONSENSUS_PORT:-30400}\"\n",
        "rport=\"${OUTBE_RETH_P2P_PORT:-30303}\"\n",
        "radport=\"${OUTBE_RADICLE_PORT:-8776}\"\n",
        "if command -v ss >/dev/null 2>&1; then\n",
        "    ss_out=\"$(ss -H -tnap 2>/dev/null | grep -E \"outbe-chain|outbe-radicle|:(${cport}|${rport}|${radport})([^0-9]|$)\" | head -n 50 || true)\"\n",
        "    if [ -n \"$ss_out\" ]; then\n",
        "        printf '%s\\n' \"$ss_out\" | tr -s ' ' | emit_lines \"ss:\"\n",
        "    else\n",
        "        emit \"ss: (no matching sockets)\"\n",
        "    fi\n",
        "else\n",
        "    emit \"ss: unavailable\"\n",
        "fi\n",
        "\n",
        "metrics_url=\"${OUTBE_DIAG_METRICS_URL:-}\"\n",
        "metrics_body=\"\"\n",
        "if [ -n \"$metrics_url\" ]; then\n",
        "    metrics_body=\"$(curl -fsS --max-time 3 \"$metrics_url\" 2>/dev/null || true)\"\n",
        "else\n",
        "    if [ -n \"${OUTBE_VM_IP:-}\" ]; then\n",
        "        metrics_body=\"$(curl -fsS --max-time 3 \"http://${OUTBE_VM_IP}:9101/metrics\" 2>/dev/null || true)\"\n",
        "    fi\n",
        "    if [ -z \"$metrics_body\" ]; then\n",
        "        metrics_body=\"$(curl -fsS --max-time 3 \"http://127.0.0.1:9101/metrics\" 2>/dev/null || true)\"\n",
        "    fi\n",
        "fi\n",
        "if [ -n \"$metrics_body\" ]; then\n",
        "    filtered_metrics=\"$(printf '%s\\n' \"$metrics_body\" | grep -v '^#' | grep -E 'reth_sync_caching|cache_size|jemalloc|process_' | head -n 40 || true)\"\n",
        "    if [ -n \"$filtered_metrics\" ]; then\n",
        "        printf '%s\\n' \"$filtered_metrics\" | emit_lines \"metrics:\"\n",
        "    else\n",
        "        emit \"metrics: (no matching lines)\"\n",
        "    fi\n",
        "else\n",
        "    emit \"metrics: unavailable\"\n",
        "fi\n",
        "\n",
        "emit \"=== END ===\"\n",
    )
    .to_owned()
}

fn write_overlay_file(path: &Path, content: &str, mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn write_overlay_symlink(path: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::symlink_metadata(path).is_ok() {
        fs::remove_file(path)?;
    }
    std::os::unix::fs::symlink(target, path)?;
    Ok(())
}

#[cfg(test)]
mod tests;
