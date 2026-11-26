use super::*;
use crate::command::run_child_with_stdin;

#[test]
fn ssh_options_passes_dash_f_only_when_configured() {
    let endpoint = Ssh {
        host: "192.0.2.50".into(),
        user: "ubuntu".into(),
        port: 2222,
    };

    // Default leaves `-F` unset.
    let default_opts = SshOptions::new(None, None).unwrap();
    let mut ssh_cmd = Command::new("ssh");
    default_opts.ssh_endpoint_args_with_control(&mut ssh_cmd, &endpoint, None);
    let ssh_args: Vec<_> = ssh_cmd.get_args().collect();
    assert!(!ssh_args.contains(&std::ffi::OsStr::new("-F")));

    let mut scp_cmd = Command::new("scp");
    default_opts.scp_endpoint_args_with_control(&mut scp_cmd, &endpoint, None);
    let scp_args: Vec<_> = scp_cmd.get_args().collect();
    assert!(!scp_args.contains(&std::ffi::OsStr::new("-F")));

    // Explicit `--ssh-config /dev/null` is accepted and passes `-F /dev/null`.
    let null_opts = SshOptions::new(None, Some(Path::new("/dev/null"))).unwrap();
    let mut ssh_cmd = Command::new("ssh");
    null_opts.ssh_endpoint_args_with_control(&mut ssh_cmd, &endpoint, None);
    let ssh_args: Vec<_> = ssh_cmd.get_args().collect();
    assert!(ssh_args.windows(2).any(|w| {
        w[0] == std::ffi::OsStr::new("-F") && w[1] == std::ffi::OsStr::new("/dev/null")
    }));

    let mut scp_cmd = Command::new("scp");
    null_opts.scp_endpoint_args_with_control(&mut scp_cmd, &endpoint, None);
    let scp_args: Vec<_> = scp_cmd.get_args().collect();
    assert!(scp_args.windows(2).any(|w| {
        w[0] == std::ffi::OsStr::new("-F") && w[1] == std::ffi::OsStr::new("/dev/null")
    }));

    // Missing config path fails closed.
    assert!(SshOptions::new(None, Some(Path::new("/nonexistent/outbe-ssh-config"))).is_err());
}

#[test]
fn host_exec_trait_supports_object_safe_fake_and_json_helper() {
    struct FakeExec;
    impl HostExec for FakeExec {
        fn capture(&self, _ssh: &Ssh, _label: &str, command: &str) -> Result<String> {
            Ok(command.trim().to_owned())
        }
        fn capture_raw(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<String> {
            Ok(r#"{"name":"outbe-up","version":"0.1.0"}"#.to_owned())
        }
        fn capture_stdin(
            &self,
            _ssh: &Ssh,
            _label: &str,
            _command: &str,
            stdin_bytes: &[u8],
        ) -> Result<String> {
            Ok(String::from_utf8_lossy(stdin_bytes).into_owned())
        }
        fn run(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<()> {
            Ok(())
        }
        fn run_quiet(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<()> {
            Ok(())
        }
        fn copy_to(
            &self,
            _ssh: &Ssh,
            _label: &str,
            _sources: &[&Path],
            _destination: &str,
            _recursive: bool,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, Deserialize, Eq, PartialEq)]
    struct Resp {
        name: String,
        version: String,
    }

    let exec: Arc<dyn HostExec> = Arc::new(FakeExec);
    let ssh = Ssh {
        host: "192.0.2.50".into(),
        user: "ubuntu".into(),
        port: 22,
    };
    let parsed: Resp =
        run_host_json(exec.as_ref(), &ssh, "amd", "outbe-up version --json").unwrap();
    assert_eq!(
        parsed,
        Resp {
            name: "outbe-up".into(),
            version: "0.1.0".into(),
        }
    );
    assert_eq!(
        exec.capture_stdin(&ssh, "amd", "cat", b"hello").unwrap(),
        "hello"
    );
}

#[test]
fn ssh_control_session_uses_0700_dir_short_socket_and_control_master_args() {
    let session = SshControlSession::new(None, Some(Path::new("/dev/null"))).unwrap();
    assert_eq!(
        fs::metadata(session.control_dir())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let endpoint = Ssh {
        host: "192.0.2.50".into(),
        user: "ubuntu".into(),
        port: 2222,
    };
    let sock = session.socket_for(&endpoint);
    assert!(sock.starts_with(session.control_dir()));
    assert!(
        sock.to_string_lossy().len() < 100,
        "socket path too long: {}",
        sock.display()
    );
    assert_eq!(sock, session.socket_for(&endpoint));

    let mut ssh_cmd = Command::new("ssh");
    session
        .options
        .ssh_endpoint_args_with_control(&mut ssh_cmd, &endpoint, Some(&sock));
    let ssh_args: Vec<String> = ssh_cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(ssh_args.contains(&"ControlMaster=auto".to_owned()));
    assert!(ssh_args.contains(&"ControlPersist=60s".to_owned()));
    assert!(ssh_args.contains(&format!("ControlPath={}", sock.display())));

    let mut scp_cmd = Command::new("scp");
    session
        .options
        .scp_endpoint_args_with_control(&mut scp_cmd, &endpoint, Some(&sock));
    let scp_args: Vec<String> = scp_cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(scp_args.contains(&"ControlMaster=auto".to_owned()));
    assert!(scp_args.contains(&format!("ControlPath={}", sock.display())));

    session.close_all();
}

#[test]
fn run_child_with_stdin_surfaces_remote_stderr_on_early_exit_broken_pipe() {
    let child = Command::new("sh")
        .args(["-c", "echo 'outbe-up: staging lock busy' >&2; exit 1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Large payload exceeds OS pipe buffer so writer hits BrokenPipe when child exits early.
    let large_stdin = vec![b'x'; 512 * 1024];
    let err = run_child_with_stdin(child, "host-a", &large_stdin).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("outbe-up: staging lock busy"),
        "expected remote stderr to be surfaced, got: {msg}"
    );
}

#[test]
fn network_known_hosts_pins_accept_new_alias_and_permissions() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("outbe-net-up");
    let pin = prepare_known_hosts_in(
        &state_dir,
        "0123456789abcdef999999999999999999999999999999999999999999999999",
    )
    .unwrap();
    assert_eq!(pin.identity, "0123456789abcdef");
    assert_eq!(pin.path, state_dir.join("known-hosts-0123456789abcdef"));
    assert_eq!(
        fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&pin.path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let key_file = tmp.path().join("id_ed25519");
    fs::write(&key_file, b"fake-key").unwrap();

    let endpoint = Ssh {
        host: "198.51.100.10".into(),
        user: "ubuntu".into(),
        port: 2222,
    };
    let opts = SshOptions::new(Some(&key_file), None)
        .unwrap()
        .with_known_hosts_file(pin.path.clone(), &pin.identity)
        .unwrap();

    let mut ssh_cmd = Command::new("ssh");
    opts.ssh_endpoint_args_with_control(&mut ssh_cmd, &endpoint, None);
    let ssh_args: Vec<String> = ssh_cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(ssh_args.contains(&"StrictHostKeyChecking=accept-new".to_owned()));
    assert!(ssh_args.contains(&format!("UserKnownHostsFile={}", pin.path.display())));
    assert!(ssh_args.contains(&"HostKeyAlias=outbe-0123456789abcdef-198.51.100.10-2222".to_owned()));
    assert!(ssh_args.contains(&"IdentitiesOnly=yes".to_owned()));

    let mut scp_cmd = Command::new("scp");
    opts.scp_endpoint_args_with_control(&mut scp_cmd, &endpoint, None);
    let scp_args: Vec<String> = scp_cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(scp_args.contains(&"StrictHostKeyChecking=accept-new".to_owned()));
    assert!(scp_args.contains(&format!("UserKnownHostsFile={}", pin.path.display())));
    assert!(scp_args.contains(&"HostKeyAlias=outbe-0123456789abcdef-198.51.100.10-2222".to_owned()));
    assert!(scp_args.contains(&"IdentitiesOnly=yes".to_owned()));

    // Without --ssh-key, IdentitiesOnly is not forced so agent/default identities still work.
    let agent_opts = SshOptions::new(None, None)
        .unwrap()
        .with_known_hosts_file(pin.path, &pin.identity)
        .unwrap();
    let mut agent_cmd = Command::new("ssh");
    agent_opts.ssh_endpoint_args_with_control(&mut agent_cmd, &endpoint, None);
    assert!(!agent_cmd.get_args().any(|arg| arg == "IdentitiesOnly=yes"));
}
