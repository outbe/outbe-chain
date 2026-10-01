//! Scenario-owned MongoDB service. No external URI or database is accepted.
use super::reject_symlink_path;
use crate::internal::config::Config;
use eyre::{bail, Result};
use mongodb::{bson::doc, sync::Client};
use outbe_offchain_storage::MongoStorageConfig;
use std::{
    collections::BTreeMap,
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

struct OwnedMongo(Child);
impl Drop for OwnedMongo {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn services() -> &'static Mutex<BTreeMap<PathBuf, OwnedMongo>> {
    static SERVICES: OnceLock<Mutex<BTreeMap<PathBuf, OwnedMongo>>> = OnceLock::new();
    SERVICES.get_or_init(Mutex::default)
}
fn root(cfg: &Config) -> PathBuf {
    cfg.dir.join("projection-mongo")
}
fn uri(port: u16) -> String {
    format!("mongodb://127.0.0.1:{port}/?replicaSet=outbe_e2e&directConnection=true")
}
fn port(cfg: &Config) -> Result<u16> {
    let file = root(cfg).join("port");
    reject_symlink_path(&file)?;
    Ok(fs::read_to_string(file)?.trim().parse()?)
}
pub(super) fn config(cfg: &Config, index: usize) -> Result<MongoStorageConfig> {
    Ok(MongoStorageConfig {
        uri: uri(port(cfg)?),
        database: format!("e2e_{}_s{}_v{}", cfg.run_tag, cfg.scenario, index),
    })
}
pub(super) fn ensure(cfg: &Config) -> Result<()> {
    let cfg = cfg.clone();
    std::thread::spawn(move || ensure_service(&cfg))
        .join()
        .map_err(|_| eyre::eyre!("scenario Mongo bootstrap thread panicked"))?
}
fn ensure_service(cfg: &Config) -> Result<()> {
    let directory = root(cfg);
    reject_symlink_path(&directory)?;
    let mut owned = services().lock().expect("Mongo service registry");
    if let Some(service) = owned.get_mut(&directory) {
        if service.0.try_wait()?.is_some() {
            bail!("scenario MongoDB exited; inspect projection-mongo/mongod.log");
        }
        return Ok(());
    }
    fs::create_dir_all(directory.join("data"))?;
    let port_file = directory.join("port");
    let port = if port_file.exists() {
        port(cfg)?
    } else {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let allocated = listener.local_addr()?.port();
        fs::write(&port_file, allocated.to_string())?;
        allocated
    };
    let process = Command::new("mongod")
        .args([
            "--bind_ip",
            "127.0.0.1",
            "--nounixsocket",
            "--replSet",
            "outbe_e2e",
            "--port",
            &port.to_string(),
        ])
        .arg("--dbpath")
        .arg(directory.join("data"))
        .arg("--logpath")
        .arg(directory.join("mongod.log"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut service = OwnedMongo(process);
    let client = Client::with_uri_str(format!(
        "mongodb://127.0.0.1:{port}/?directConnection=true&serverSelectionTimeoutMS=500"
    ))?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut initialized = false;
    loop {
        if service.0.try_wait()?.is_some() {
            bail!("scenario MongoDB exited during bootstrap");
        }
        if let Ok(hello) = client
            .database("admin")
            .run_command(doc! { "hello": 1 })
            .run()
        {
            if hello.get_bool("isWritablePrimary").unwrap_or(false) {
                break;
            }
            if !initialized {
                // A restart preserves the exact replica set and its data.
                let result = client.database("admin").run_command(doc! { "replSetInitiate": { "_id": "outbe_e2e", "members": [{ "_id": 0, "host": format!("127.0.0.1:{port}") }] } }).run();
                if result.is_ok()
                    || result
                        .as_ref()
                        .err()
                        .is_some_and(|e| e.to_string().contains("already initialized"))
                {
                    initialized = true;
                }
            }
        }
        if Instant::now() >= deadline {
            bail!("scenario MongoDB replica set did not become writable");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    owned.insert(directory, service);
    Ok(())
}
pub(super) fn reset(cfg: &Config, config: &MongoStorageConfig) -> Result<()> {
    let cfg = cfg.clone();
    let config = config.clone();
    std::thread::spawn(move || reset_database(&cfg, &config))
        .join()
        .map_err(|_| eyre::eyre!("scenario Mongo reset thread panicked"))?
}
fn reset_database(cfg: &Config, config: &MongoStorageConfig) -> Result<()> {
    // All callers first compare config against the scenario-generated exact identity.
    if config.uri != uri(port(cfg)?)
        || !config
            .database
            .starts_with(&format!("e2e_{}_s{}_v", cfg.run_tag, cfg.scenario))
    {
        bail!("Mongo reset identity is outside this scenario");
    }
    Client::with_uri_str(&config.uri)?
        .database(&config.database)
        .drop()
        .run()?;
    Ok(())
}
pub(super) fn stop(cfg: &Config) {
    services()
        .lock()
        .expect("Mongo service registry")
        .remove(&root(cfg));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{Environment, ProjectionBackend};
    use outbe_offchain_storage::{Key, Namespace, StorageProvider, Value};

    #[tokio::test]
    #[ignore = "requires local mongod and loopback socket access"]
    async fn managed_service_can_be_started_and_reset_from_the_async_scenario_runner() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment {
            projection_backend: ProjectionBackend::Mongodb,
            ..Environment::default()
        });
        cfg.dir = root.path().canonicalize().unwrap();
        cfg.scenario = 1;
        super::super::ensure_node_config(&cfg, 0).unwrap();
        let fixture = super::super::ProjectionFixture::new(&cfg);
        fixture
            .run(|cfg| {
                let opened = StorageProvider::new(super::super::storage_config(&cfg, 0)?)?
                    .with_partition_routing(outbe_offchain_data::entity_partition_routing()?)
                    .open_writer()?;
                opened.writer.put(
                    Namespace::new("fixture")?,
                    &Key::new([1])?,
                    &Value::new([7])?,
                )?;
                Ok(())
            })
            .unwrap();
        fixture.reset_projection_state().unwrap();
        fixture
            .run(|cfg| {
                assert!(super::super::session(&cfg, 0)?
                    .get(Namespace::new("fixture")?, &Key::new([1])?)?
                    .is_none());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    #[ignore = "requires local mongod and loopback socket access"]
    fn managed_service_reuses_identity_and_data_across_restart_then_resets_only_owned_databases() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment {
            projection_backend: ProjectionBackend::Mongodb,
            ..Environment::default()
        });
        cfg.dir = root.path().canonicalize().unwrap();
        cfg.scenario = 1;
        super::super::ensure_node_config(&cfg, 0).unwrap();
        super::super::ensure_node_config(&cfg, 1).unwrap();
        let path = cfg.projection_storage_config(0);
        let original = fs::read(&path).unwrap();
        let storage = super::super::storage_config(&cfg, 0).unwrap();
        let ns = Namespace::new("fixture").unwrap();
        let key = Key::new([1]).unwrap();
        {
            let opened = StorageProvider::new(storage)
                .unwrap()
                .with_partition_routing(outbe_offchain_data::entity_partition_routing().unwrap())
                .open_writer()
                .unwrap();
            opened
                .writer
                .put(ns.clone(), &key, &Value::new([7]).unwrap())
                .unwrap();
        }
        stop(&cfg);
        super::super::ensure_node_config(&cfg, 0).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            super::super::session(&cfg, 0)
                .unwrap()
                .get(ns.clone(), &key)
                .unwrap()
                .unwrap()
                .as_bytes(),
            &[7]
        );
        let config = super::super::storage_config(&cfg, 0).unwrap();
        let StorageBackend::MongoDb(mongo) = config.backend else {
            unreachable!()
        };
        reset(&cfg, &mongo).unwrap();
        assert!(super::super::session(&cfg, 0)
            .unwrap()
            .get(ns, &key)
            .unwrap()
            .is_none());
        stop(&cfg);
    }
    use outbe_offchain_storage::StorageBackend;
}
