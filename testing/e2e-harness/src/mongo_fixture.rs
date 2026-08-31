//! Rust-owned MongoDB replica-set fixture shared by integration lanes.

use std::{net::TcpListener, process::Stdio, thread::sleep, time::Duration};

use eyre::{bail, Result, WrapErr};

use crate::internal::proc::{base_cmd, docker_rm, wait_tcp, DockerGuard};

const MANAGED_MONGO_IMAGE: &str = "mongo:7.0";

/// One transaction-capable MongoDB replica set owned by the calling test.
#[derive(Debug)]
pub struct ManagedMongoReplicaSet {
    uri: String,
    name: String,
    port: u16,
    sudo: bool,
    #[allow(dead_code)]
    guard: DockerGuard,
}

impl ManagedMongoReplicaSet {
    /// Start an isolated replica set and remove it automatically on drop.
    pub fn start(scope: &str, sudo: bool) -> Result<Self> {
        if scope.is_empty()
            || scope.len() > 48
            || scope
                .bytes()
                .any(|byte| !byte.is_ascii_alphanumeric() && byte != b'-')
        {
            bail!("managed MongoDB scope must be 1..=48 ASCII alphanumeric/hyphen bytes");
        }
        Self::start_named(
            &format!("outbe-e2e-mongodb-{scope}-{}", std::process::id()),
            sudo,
        )
    }

    pub(crate) fn start_named(name: &str, sudo: bool) -> Result<Self> {
        let port = free_tcp_port()?;
        docker_rm(name, sudo);

        // Publish the port instead of `--network host`: host networking does not
        // reach the host loopback on Docker Desktop (Linux VM), so the fixture must
        // work whether the daemon is native Linux or a VM. `--bind_ip_all` lets the
        // docker port proxy reach mongod on the container interface.
        let output = base_cmd("docker", sudo)
            .args(managed_mongo_run_args(name, port))
            .output()
            .wrap_err("start managed MongoDB container")?;
        if !output.status.success() {
            bail!(
                "start managed MongoDB container: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let guard = DockerGuard::new(name, sudo);
        if !wait_tcp(port, 200) {
            bail!("managed MongoDB did not listen on 127.0.0.1:{port}");
        }

        // Initiate the set (ignore "already initialized" on retries) and only
        // report ready once the node has actually won its election - rs.initiate
        // returns ok:1 before the member transitions to PRIMARY, so writing early
        // races into NotWritablePrimary.
        let init = format!(
            "try {{ rs.initiate({{_id:'rs0',members:[{{_id:0,host:'127.0.0.1:{port}'}}]}}) }} catch (e) {{}} if (!db.hello().isWritablePrimary) {{ quit(1) }}"
        );
        let mut ready = false;
        for _ in 0..60 {
            let status = base_cmd("docker", sudo)
                .args([
                    "exec",
                    name,
                    "mongosh",
                    "--quiet",
                    "--port",
                    &port.to_string(),
                    "--eval",
                    &init,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if status.is_ok_and(|status| status.success()) {
                ready = true;
                break;
            }
            sleep(Duration::from_millis(250));
        }
        if !ready {
            bail!("managed MongoDB replica set initialization failed");
        }

        Ok(Self {
            uri: format!("mongodb://127.0.0.1:{port}/?replicaSet=rs0&directConnection=true"),
            name: name.to_owned(),
            port,
            sudo,
            guard,
        })
    }

    /// Transaction-capable loopback URI valid for this fixture's lifetime.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Pause the managed replica set without changing its identity or data.
    pub fn pause(&self) -> Result<()> {
        self.docker_control("pause")
    }

    /// Resume the same replica set and wait until it is writable again.
    pub fn resume(&self) -> Result<()> {
        self.docker_control("unpause")?;
        if !wait_tcp(self.port, 200) {
            bail!(
                "resumed managed MongoDB did not listen on 127.0.0.1:{}",
                self.port
            );
        }
        let mut ready = false;
        for _ in 0..60 {
            let status = base_cmd("docker", self.sudo)
                .args([
                    "exec",
                    self.name.as_str(),
                    "mongosh",
                    "--quiet",
                    "--port",
                    &self.port.to_string(),
                    "--eval",
                    "if (!db.hello().isWritablePrimary) { quit(1) }",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if status.is_ok_and(|status| status.success()) {
                ready = true;
                break;
            }
            sleep(Duration::from_millis(250));
        }
        if !ready {
            bail!("resumed managed MongoDB replica set did not become writable");
        }
        Ok(())
    }

    fn docker_control(&self, operation: &str) -> Result<()> {
        let output = base_cmd("docker", self.sudo)
            .args([operation, self.name.as_str()])
            .output()
            .wrap_err_with(|| format!("{operation} managed MongoDB container"))?;
        if !output.status.success() {
            bail!(
                "{operation} managed MongoDB container: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

fn managed_mongo_run_args(name: &str, port: u16) -> Vec<String> {
    vec![
        "run".to_owned(),
        "-d".to_owned(),
        "--name".to_owned(),
        name.to_owned(),
        "--publish".to_owned(),
        format!("127.0.0.1:{port}:{port}"),
        MANAGED_MONGO_IMAGE.to_owned(),
        "--replSet".to_owned(),
        "rs0".to_owned(),
        "--bind_ip_all".to_owned(),
        "--port".to_owned(),
        port.to_string(),
    ]
}

fn free_tcp_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).wrap_err("reserve MongoDB port")?;
    Ok(listener.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::managed_mongo_run_args;

    #[test]
    fn managed_mongo_uses_loopback_port_publishing() {
        let args = managed_mongo_run_args("scenario-mongo", 27123);

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--publish", "127.0.0.1:27123:27123"]));
        assert!(args.iter().any(|arg| arg == "--bind_ip_all"));
        assert!(!args.iter().any(|arg| arg == "--network"));
        assert!(!args.iter().any(|arg| arg == "host"));
    }
}
