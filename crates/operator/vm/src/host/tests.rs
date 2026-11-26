use std::{net::Ipv4Addr, path::Path};

use super::{
    dnsmasq_argv::parse_dnsmasq_argv,
    probe::{extract_systemd_exec_argv, parse_systemd_show_props},
    *,
};
use crate::{dnsmasq::validate_dnsmasq_hosts_format, nat::Ipv4Subnet};

const SAMPLE_AMD_NFT: &str = r#"{
  "nftables": [
    {"metainfo": {"version": "1.1.6", "json_schema_version": 1}},
    {"table": {"family": "inet", "name": "host_firewall", "handle": 66}},
    {"chain": {"family": "inet", "table": "host_firewall", "name": "forward", "handle": 2, "type": "filter", "hook": "forward", "prio": 0, "policy": "drop"}},
    {"rule": {"family": "inet", "table": "host_firewall", "chain": "forward", "handle": 700, "comment": "outbound:validator_vms", "expr": [
      {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
      {"accept": null}
    ]}},
    {"rule": {"family": "inet", "table": "host_firewall", "chain": "forward", "handle": 711, "comment": "validator-0-hyperlane", "expr": [
      {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": "198.51.100.30"}},
      {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "10.77.0.11"}},
      {"match": {"op": "in", "left": {"ct": {"key": "status"}}, "right": "dnat"}},
      {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": 9000}},
      {"accept": null}
    ]}},
    {"table": {"family": "ip", "name": "host_firewall_nat", "handle": 67}},
    {"chain": {"family": "ip", "table": "host_firewall_nat", "name": "prerouting", "handle": 1, "type": "nat", "hook": "prerouting", "prio": -100, "policy": "accept"}},
    {"chain": {"family": "ip", "table": "host_firewall_nat", "name": "postrouting", "handle": 2, "type": "nat", "hook": "postrouting", "prio": 100, "policy": "accept"}},
    {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 456, "comment": "validator-0-hyperlane", "expr": [
      {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "198.51.100.10"}},
      {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": "198.51.100.30"}},
      {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": 9000}},
      {"dnat": {"addr": "10.77.0.11"}}
    ]}},
    {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 466, "comment": "validator-1-hyperlane", "expr": [
      {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "198.51.100.10"}},
      {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": "198.51.100.30"}},
      {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": 9001}},
      {"dnat": {"addr": "10.77.0.12", "port": 9000}}
    ]}},
    {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "postrouting", "handle": 468, "comment": "outbound:validator_vms", "expr": [
      {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
      {"match": {"op": "!=", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
      {"masquerade": null}
    ]}}
  ]
}"#;

#[test]
fn parse_firewall_json_extracts_dnat_forward_allows_and_egress() {
    let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
    let (fw, egress, partial) =
        parse_firewall_and_egress_json(SAMPLE_AMD_NFT, "obr0", Some(subnet), Some("eno1")).unwrap();
    assert!(fw.nft_accessible);
    assert!(!fw.managed_nat_table_present);
    assert_eq!(fw.tables.len(), 2);
    assert_eq!(fw.forward_chains.len(), 1);
    assert_eq!(fw.forward_chains[0].policy, "drop");
    assert_eq!(fw.forward_allows.len(), 1);
    assert_eq!(fw.forward_allows[0].dest_ip, Ipv4Addr::new(10, 77, 0, 11));
    assert_eq!(fw.forward_allows[0].dest_port, 9000);
    assert_eq!(fw.forward_allows[0].sources, vec!["198.51.100.30/32"]);

    assert_eq!(fw.dnat_rules.len(), 2);
    assert_eq!(fw.dnat_rules[0].host_port, 9000);
    assert_eq!(fw.dnat_rules[0].guest_port, 9000);
    assert_eq!(fw.dnat_rules[0].dest_ip, Ipv4Addr::new(10, 77, 0, 11));
    assert_eq!(
        fw.dnat_rules[0].public_ip,
        Some(Ipv4Addr::new(198, 51, 100, 10))
    );
    assert_eq!(fw.dnat_rules[1].host_port, 9001);
    assert_eq!(fw.dnat_rules[1].guest_port, 9000);
    assert_eq!(fw.dnat_rules[1].dest_ip, Ipv4Addr::new(10, 77, 0, 12));

    assert_eq!(egress.len(), 1);
    assert_eq!(egress[0].table, "host_firewall_nat");
    assert!(partial.is_empty());
}

#[test]
fn parse_systemd_dnsmasq_argv_matches_real_validator_vm_dhcp_unit() {
    let raw = "\
LoadState=loaded
ActiveState=active
UnitFileState=enabled
MainPID=219283
ExecStart={ path=/usr/sbin/dnsmasq ; argv[]=/usr/sbin/dnsmasq --user=root --conf-file=/dev/null --interface=obr0 --bind-interfaces --except-interface=lo --port=0 --dhcp-ignore-clid --dhcp-range=10.77.0.100,10.77.0.200,255.255.255.0,5m --dhcp-hostsfile=/var/lib/validator-vm/dhcp-hosts --dhcp-option=option:router,10.77.0.1 --dhcp-leasefile=/var/lib/validator-vm/dhcp-leases --pid-file=/run/validator-vm-dhcp.pid ; ignore_errors=no }
ExecReload={ path=/bin/kill ; argv[]=/bin/kill -HUP $MAINPID ; ignore_errors=no }
";
    let props = parse_systemd_show_props(raw);
    assert!(props.is_loaded());
    assert!(props.is_active());
    assert!(props.is_enabled());
    assert_eq!(props.main_pid, Some(219283));
    let argv = extract_systemd_exec_argv(&props.exec_start);
    let parsed = parse_dnsmasq_argv(&argv);
    assert_eq!(parsed.interface.as_deref(), Some("obr0"));
    assert_eq!(
        parsed.hosts_file.as_deref(),
        Some(Path::new("/var/lib/validator-vm/dhcp-hosts"))
    );
    assert_eq!(
        parsed.leases_file.as_deref(),
        Some(Path::new("/var/lib/validator-vm/dhcp-leases"))
    );
    assert_eq!(
        parsed.pid_file.as_deref(),
        Some(Path::new("/run/validator-vm-dhcp.pid"))
    );
    assert_eq!(parsed.pool_start, Some(Ipv4Addr::new(10, 77, 0, 100)));
    assert_eq!(parsed.pool_end, Some(Ipv4Addr::new(10, 77, 0, 200)));
    assert_eq!(parsed.netmask, Some(Ipv4Addr::new(255, 255, 255, 0)));
    assert_eq!(parsed.lease_duration.as_deref(), Some("5m"));
}

#[test]
fn port_spec_and_cidr_parsers_validate_inputs() {
    assert_eq!(
        parse_port_spec("9000").unwrap(),
        PublishPortSpec {
            guest_port: 9000,
            host_port: 9000,
            proto: TransportProto::Tcp,
        }
    );
    assert_eq!(
        parse_port_spec("9000:9010").unwrap(),
        PublishPortSpec {
            guest_port: 9000,
            host_port: 9010,
            proto: TransportProto::Tcp,
        }
    );
    assert_eq!(
        parse_port_spec("30303:30313/udp").unwrap(),
        PublishPortSpec {
            guest_port: 30303,
            host_port: 30313,
            proto: TransportProto::Udp,
        }
    );
    assert!(parse_port_spec("0").is_err());
    assert!(parse_port_spec("9000/icmp").is_err());

    assert_eq!(
        normalize_ipv4_cidr("198.51.100.30").unwrap(),
        "198.51.100.30/32"
    );
    assert_eq!(normalize_ipv4_cidr("10.77.0.5/24").unwrap(), "10.77.0.0/24");
    assert_eq!(normalize_ipv4_cidr("0.0.0.0/0").unwrap(), "0.0.0.0/0");
    assert!(normalize_ipv4_cidr("10.77.0.0/33").is_err());
}

#[test]
fn validate_real_amd_and_intel_dhcp_hosts_formats() {
    let amd_hosts = "\
be:6a:63:f8:84:d0,10.77.0.11,validator-0,infinite
5a:9a:a8:0f:91:46,10.77.0.12,validator-1,infinite

# outbe-up:d3-amd-73d044094:begin
ca:ae:de:a3:b1:11,10.77.0.60
# outbe-up:d3-amd-73d044094:end

# outbe-up:final-amd-f36afc309:begin
be:f7:7b:2c:d1:85,10.77.0.68,10m
# outbe-up:final-amd-f36afc309:end
";
    assert_eq!(validate_dnsmasq_hosts_format(amd_hosts).unwrap(), 4);
    assert!(validate_dnsmasq_hosts_format("not a valid dhcp hosts file\n").is_err());
}

#[test]
fn f4_expands_dnat_port_ranges_and_sets_and_records_unparsed_dnat_rules() {
    let nft_json = r#"{
      "nftables": [
        {"metainfo": {"version": "1.1.6", "json_schema_version": 1}},
        {"table": {"family": "ip", "name": "host_firewall_nat", "handle": 67}},
        {"chain": {"family": "ip", "table": "host_firewall_nat", "name": "prerouting", "handle": 1, "type": "nat", "hook": "prerouting", "prio": -100, "policy": "accept"}},
        {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 10, "comment": "range-dnat", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "198.51.100.10"}},
          {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": {"range": [9000, 9010]}}},
          {"dnat": {"addr": "10.77.0.11"}}
        ]}},
        {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 11, "comment": "set-dnat", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "198.51.100.10"}},
          {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": {"set": [9020, 9021, {"range": [9030, 9031]}]}}},
          {"dnat": {"addr": "10.77.0.12"}}
        ]}},
        {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 12, "comment": "huge-range", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "udp", "field": "dport"}}, "right": {"range": [20000, 25000]}}},
          {"dnat": {"addr": "10.77.0.13"}}
        ]}},
        {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 20, "comment": "named-set", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": "@dynamic_ports"}},
          {"dnat": {"addr": "10.77.0.14"}}
        ]}},
        {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 21, "comment": "dnat-map", "expr": [
          {"dnat": {"addr": {"map": {"key": {"payload": {"protocol": "tcp", "field": "dport"}}, "data": "@port_map"}}}}
        ]}},
        {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 22, "comment": "negated-dport", "expr": [
          {"match": {"op": "!=", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": 22}},
          {"dnat": {"addr": "10.77.0.15"}}
        ]}},
        {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 23, "comment": "wildcard-dnat", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "198.51.100.10"}},
          {"dnat": {"addr": "10.77.0.16"}}
        ]}}
      ]
    }"#;

    let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
    let (fw, _, _) =
        parse_firewall_and_egress_json(nft_json, "obr0", Some(subnet), Some("eno1")).unwrap();

    // 11 (9000..=9010) + 4 (9020, 9021, 9030, 9031) + 1 (compact 20000..=25000) = 16
    assert_eq!(fw.dnat_rules.len(), 16);
    assert!(fw.dnat_rules.iter().any(|r| r.proto == "tcp"
        && r.covers_host_port(9005)
        && r.dest_ip == Ipv4Addr::new(10, 77, 0, 11)));
    assert!(fw.dnat_rules.iter().any(|r| r.proto == "tcp"
        && r.covers_host_port(9021)
        && r.dest_ip == Ipv4Addr::new(10, 77, 0, 12)));
    assert!(fw.dnat_rules.iter().any(|r| r.proto == "tcp"
        && r.covers_host_port(9031)
        && r.dest_ip == Ipv4Addr::new(10, 77, 0, 12)));
    assert!(fw.dnat_rules.iter().any(|r| {
        r.proto == "udp"
            && r.host_port == 20000
            && r.host_port_end == Some(25000)
            && r.covers_host_port(22500)
            && !r.covers_host_port(25001)
    }));

    // All 4 unparseable DNAT rules must be recorded explicitly.
    assert_eq!(fw.unparsed_dnat_rules.len(), 4);
    let handles: Vec<Option<u64>> = fw.unparsed_dnat_rules.iter().map(|u| u.handle).collect();
    assert_eq!(handles, vec![Some(20), Some(21), Some(22), Some(23)]);
}
