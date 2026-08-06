use std::fs;

use outbe_e2e_harness::ocomp_capacity::OcompCapacityResourceMeterV1;

#[test]
fn capacity_meter_reports_checked_deltas_and_exact_cas_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let cgroup = temp.path().join("cgroup");
    let network = temp.path().join("network");
    let cas = temp.path().join("cas");
    fs::create_dir_all(&cgroup).unwrap();
    fs::create_dir_all(&network).unwrap();
    fs::create_dir_all(&cas).unwrap();
    fs::write(cgroup.join("memory.max"), "12000\n").unwrap();
    fs::write(cgroup.join("cpu.max"), "400000 100000\n").unwrap();
    fs::write(cgroup.join("cpu.stat"), "usage_usec 100\n").unwrap();
    fs::write(cgroup.join("memory.peak"), "1000\n").unwrap();
    fs::write(
        cgroup.join("io.stat"),
        "8:0 rbytes=10 wbytes=200 rios=1 wios=2\n",
    )
    .unwrap();
    fs::write(network.join("rx_bytes"), "30\n").unwrap();
    fs::write(network.join("tx_bytes"), "40\n").unwrap();

    let meter = OcompCapacityResourceMeterV1::start(&cgroup, &network).unwrap();

    fs::write(cgroup.join("cpu.stat"), "usage_usec 250\n").unwrap();
    fs::write(cgroup.join("memory.peak"), "1600\n").unwrap();
    fs::write(
        cgroup.join("io.stat"),
        "8:0 rbytes=20 wbytes=500 rios=2 wios=5\n",
    )
    .unwrap();
    fs::write(network.join("rx_bytes"), "60\n").unwrap();
    fs::write(network.join("tx_bytes"), "90\n").unwrap();
    fs::write(cas.join("object"), vec![7_u8; 50]).unwrap();

    let observed = meter.finish(&[cas]).unwrap();
    assert_eq!(observed.cgroup_path, cgroup.to_string_lossy());
    assert!(observed.resource_cgroup_writable);
    assert_eq!(observed.memory_limit_bytes, 12000);
    assert_eq!(observed.cpu_quota_micros, 400000);
    assert_eq!(observed.cpu_period_micros, 100000);
    assert_eq!(observed.cpu_micros, 150);
    assert_eq!(observed.assigned_memory_bytes, 1600);
    assert_eq!(observed.disk_write_bytes, 300);
    assert_eq!(observed.network_bytes, 80);
    assert_eq!(observed.cas_bytes, 50);
}
