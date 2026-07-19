//! Projection MongoDB owned by a scenario unless the caller supplies a URI.

use std::net::TcpListener;
use std::process::Stdio;
use std::thread::sleep;
use std::time::Duration;

use eyre::{bail, eyre, Result, WrapErr};
use mongodb::bson::{doc, Bson, Document};
use mongodb::sync::Client;

use crate::env::Environment;
use crate::internal::config::Config;
use crate::internal::proc::{base_cmd, docker_rm, wait_tcp, DockerGuard};

const MANAGED_MONGO_IMAGE: &str = "mongo:7.0";
const COLLECTIONS: [&str; 3] = ["tributes", "tributes_by_owner", "tributes_by_day"];

/// Scenario-scoped projection store and its managed-container lifetime guard.
#[derive(Debug)]
pub struct MongoDb {
    uri: String,
    database_prefix: String,
    scenario: usize,
    validators: usize,
    #[allow(dead_code)]
    guard: Option<DockerGuard>,
}

/// Exact primary projection value needed to verify one compressed Tribute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedTribute {
    pub raw_id: outbe_compressed_entities::EntityId36,
    pub stored_body: Vec<u8>,
}

impl MongoDb {
    /// Use the configured URI, or replace `auto` with an owned replica set.
    pub(crate) fn connect_or_start(cfg: &mut Config) -> Result<Self> {
        let guard = if cfg.projection_mongodb_uri == "auto" {
            let (uri, guard) = start_replica_set(cfg)?;
            cfg.projection_mongodb_uri = uri;
            Some(guard)
        } else {
            None
        };
        Ok(Self {
            uri: cfg.projection_mongodb_uri.clone(),
            database_prefix: cfg.projection_database_prefix.clone(),
            scenario: cfg.scenario,
            validators: cfg.validators,
            guard,
        })
    }

    /// Signal-path backstop for managed containers. Normal scenario teardown is
    /// handled by `DockerGuard`; process exit does not run destructors.
    pub(crate) fn teardown_managed_for_run(env: &Environment) {
        if env.projection_mongodb_uri != "auto" {
            return;
        }
        let cfg = Config::resolve(env);
        let prefix = format!("outbe-e2e-mongodb-{}-", cfg.run_tag);
        let Ok(output) = base_cmd("docker", cfg.sudo)
            .args(["ps", "-aq", "--filter", &format!("name={prefix}")])
            .output()
        else {
            return;
        };
        for id in String::from_utf8_lossy(&output.stdout).split_whitespace() {
            docker_rm(id, cfg.sudo);
        }
    }

    /// Wait for all three tribute namespaces in every validator database, then
    /// assert the complete BSON documents are identical across the committee.
    pub fn wait_for_tribute_projection(&self, tx_hash: &str, tries: u32) -> Result<()> {
        let uri = self.uri.clone();
        let database_prefix = self.database_prefix.clone();
        let scenario = self.scenario;
        let validators = self.validators;
        let tx_hash = tx_hash.to_owned();
        std::thread::spawn(move || {
            wait_for_projection(
                &uri,
                &database_prefix,
                scenario,
                validators,
                &tx_hash,
                tries,
            )
        })
        .join()
        .map_err(|_| eyre!("projection MongoDB worker panicked"))?
    }

    /// Load the exact authenticated identity/body bytes projected by one validator.
    pub fn projected_tribute(&self, validator: usize, tx_hash: &str) -> Result<ProjectedTribute> {
        let uri = self.uri.clone();
        let name = format!(
            "{}_scenario_{}_validator-{validator}",
            self.database_prefix, self.scenario
        );
        let tx_hash = tx_hash.to_owned();
        std::thread::spawn(move || projected_tribute(&uri, &name, &tx_hash))
            .join()
            .map_err(|_| eyre!("projection MongoDB worker panicked"))?
    }

    /// Assert that no Tribute primary or secondary projection exists anywhere.
    pub fn assert_no_tribute_projection(&self) -> Result<()> {
        let uri = self.uri.clone();
        let database_prefix = self.database_prefix.clone();
        let scenario = self.scenario;
        let validators = self.validators;
        std::thread::spawn(move || {
            let client = Client::with_uri_str(&uri).wrap_err("connect projection MongoDB")?;
            for validator in 0..validators {
                let name = format!("{database_prefix}_scenario_{scenario}_validator-{validator}");
                let db = client.database(&name);
                for collection_name in COLLECTIONS {
                    let count = db
                        .collection::<Document>(collection_name)
                        .count_documents(doc! {})
                        .run()?;
                    if count != 0 {
                        bail!("{name}.{collection_name}: expected no documents, found {count}");
                    }
                }
            }
            Ok(())
        })
        .join()
        .map_err(|_| eyre!("projection MongoDB worker panicked"))?
    }
}

fn projected_tribute(uri: &str, name: &str, tx_hash: &str) -> Result<ProjectedTribute> {
    let client = Client::with_uri_str(uri).wrap_err("connect projection MongoDB")?;
    let document = client
        .database(name)
        .collection::<Document>("tributes")
        .find_one(doc! {"_projection.tx_hash": tx_hash})
        .run()?
        .ok_or_else(|| eyre!("{name}.tributes has no row for transaction {tx_hash}"))?;
    let encoded_id = document
        .get_str("_id")
        .map_err(|error| eyre!("{name}.tributes has invalid _id: {error}"))?;
    let id = hex::decode(encoded_id).wrap_err("decode projected Tribute _id")?;
    let raw_id = outbe_compressed_entities::EntityId36::try_from(id.as_slice())
        .wrap_err("projected Tribute _id is not EntityId36")?;
    let stored_body = match document.get("value") {
        Some(Bson::Binary(value)) => value.bytes.clone(),
        other => return Err(eyre!("{name}.tributes has invalid value field: {other:?}")),
    };
    Ok(ProjectedTribute {
        raw_id,
        stored_body,
    })
}

fn wait_for_projection(
    uri: &str,
    database_prefix: &str,
    scenario: usize,
    validators: usize,
    tx_hash: &str,
    tries: u32,
) -> Result<()> {
    let mut last = None;
    for _ in 0..tries {
        match tribute_projection(uri, database_prefix, scenario, validators, tx_hash) {
            Ok(()) => return Ok(()),
            Err(error) => last = Some(error),
        }
        sleep(Duration::from_millis(500));
    }
    Err(last.unwrap_or_else(|| eyre!("projection did not appear")))
}

fn tribute_projection(
    uri: &str,
    database_prefix: &str,
    scenario: usize,
    validators: usize,
    tx_hash: &str,
) -> Result<()> {
    let client = Client::with_uri_str(uri).wrap_err("connect projection MongoDB")?;
    let mut canonical: Option<Vec<Document>> = None;
    for validator in 0..validators {
        let name = format!("{database_prefix}_scenario_{scenario}_validator-{validator}");
        let db = client.database(&name);
        let mut documents = Vec::with_capacity(COLLECTIONS.len());
        for collection_name in COLLECTIONS {
            let collection = db.collection::<Document>(collection_name);
            let count = collection.count_documents(doc! {}).run()?;
            if count != 1 {
                bail!("{name}.{collection_name}: expected 1 document, found {count}");
            }
            documents.push(
                collection
                    .find_one(doc! {})
                    .run()?
                    .ok_or_else(|| eyre!("{name}.{collection_name}: document disappeared"))?,
            );
        }

        let projected_tx = documents[0]
            .get_document("_projection")
            .and_then(|projection| projection.get_str("tx_hash"))
            .map_err(|error| eyre!("{name}.tributes missing _projection.tx_hash: {error}"))?;
        if !projected_tx.eq_ignore_ascii_case(tx_hash) {
            bail!("{name}.tributes projected tx {projected_tx}, expected successful tx {tx_hash}");
        }

        if let Some(expected) = &canonical {
            if &documents != expected {
                bail!("{name}: tribute projection differs from validator-0");
            }
        } else {
            canonical = Some(documents);
        }
    }
    Ok(())
}

fn start_replica_set(cfg: &Config) -> Result<(String, DockerGuard)> {
    let port = free_tcp_port()?;
    let name = format!("outbe-e2e-mongodb-{}-s{}", cfg.run_tag, cfg.scenario);
    docker_rm(&name, cfg.sudo);

    let output = base_cmd("docker", cfg.sudo)
        .args([
            "run",
            "-d",
            "--name",
            &name,
            "--network",
            "host",
            MANAGED_MONGO_IMAGE,
            "--replSet",
            "rs0",
            "--bind_ip",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ])
        .output()
        .wrap_err("start managed MongoDB container")?;
    if !output.status.success() {
        bail!(
            "start managed MongoDB container: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let guard = DockerGuard::new(&name, cfg.sudo);
    if !wait_tcp(port, 200) {
        bail!("managed MongoDB did not listen on 127.0.0.1:{port}");
    }

    let init = format!("rs.initiate({{_id:'rs0',members:[{{_id:0,host:'127.0.0.1:{port}'}}]}})");
    let mut ready = false;
    for _ in 0..60 {
        let status = base_cmd("docker", cfg.sudo)
            .args([
                "exec",
                &name,
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

    let uri = format!("mongodb://127.0.0.1:{port}/?replicaSet=rs0&directConnection=true");
    Ok((uri, guard))
}

fn free_tcp_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).wrap_err("reserve MongoDB port")?;
    Ok(listener.local_addr()?.port())
}
