//! Lifecycle owner for the node-side fixed-role OCOMP control sockets.

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use super::control::OcompControlServer;

/// Distinct paths for the two node-owned fixed-role capability surfaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompControlRuntimeConfig {
    pub supervisor_socket: PathBuf,
    pub snapshot_exporter_socket: PathBuf,
}

/// Owns both listeners, their serving threads and deterministic teardown.
pub struct OcompControlRuntime {
    shutdown: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    sockets: [PathBuf; 2],
}

impl OcompControlRuntime {
    pub fn start(
        server: Arc<OcompControlServer>,
        config: OcompControlRuntimeConfig,
    ) -> io::Result<Self> {
        if config.supervisor_socket == config.snapshot_exporter_socket {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OCOMP fixed roles require distinct node sockets",
            ));
        }

        let supervisor = bind_control_socket(&config.supervisor_socket)?;
        let exporter = match bind_control_socket(&config.snapshot_exporter_socket) {
            Ok(listener) => listener,
            Err(error) => {
                remove_owned_socket(&config.supervisor_socket);
                return Err(error);
            }
        };
        let shutdown = Arc::new(AtomicBool::new(false));
        let supervisor_thread = spawn_role_thread(
            "outbe-ocomp-node-supervisor",
            Arc::clone(&server),
            Arc::clone(&shutdown),
            supervisor,
            OcompNodeRole::Supervisor,
        )?;
        let exporter_thread = match spawn_role_thread(
            "outbe-ocomp-node-snapshot-exporter",
            server,
            Arc::clone(&shutdown),
            exporter,
            OcompNodeRole::SnapshotExporter,
        ) {
            Ok(thread) => thread,
            Err(error) => {
                shutdown.store(true, Ordering::Release);
                let _ = supervisor_thread.join();
                remove_owned_socket(&config.supervisor_socket);
                remove_owned_socket(&config.snapshot_exporter_socket);
                return Err(error);
            }
        };

        Ok(Self {
            shutdown,
            threads: vec![supervisor_thread, exporter_thread],
            sockets: [config.supervisor_socket, config.snapshot_exporter_socket],
        })
    }
}

impl Drop for OcompControlRuntime {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        for socket in &self.sockets {
            remove_owned_socket(socket);
        }
    }
}

#[derive(Clone, Copy)]
enum OcompNodeRole {
    Supervisor,
    SnapshotExporter,
}

fn spawn_role_thread(
    name: &'static str,
    server: Arc<OcompControlServer>,
    shutdown: Arc<AtomicBool>,
    listener: UnixListener,
    role: OcompNodeRole,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name(name.to_owned()).spawn(move || {
        let result = match role {
            OcompNodeRole::Supervisor => server.serve_until(listener, &shutdown),
            OcompNodeRole::SnapshotExporter => {
                server.serve_snapshot_exporter_until(listener, &shutdown)
            }
        };
        if let Err(error) = result {
            tracing::error!(role = name, %error, "node OCOMP control listener stopped");
        }
    })
}

fn bind_control_socket(path: &Path) -> io::Result<UnixListener> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "OCOMP control socket path is empty",
        ));
    }
    if fs::symlink_metadata(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("OCOMP control socket already exists: {}", path.display()),
        ));
    }
    let listener = UnixListener::bind(path)?;
    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o660)) {
        drop(listener);
        remove_owned_socket(path);
        return Err(error);
    }
    Ok(listener)
}

fn remove_owned_socket(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket()) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};

    use super::*;

    #[test]
    fn control_socket_is_private_and_never_replaces_an_existing_entry() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("node-supervisor.sock");
        let listener = bind_control_socket(&path).unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();

        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o660);
        assert_eq!(
            bind_control_socket(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );

        drop(listener);
        remove_owned_socket(&path);
        assert!(!path.exists());
    }
}
