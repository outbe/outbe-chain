use crate::{rpc::Rpc, wallet::Wallet};
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use eyre::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

pub fn private_dir(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    if !path.exists() {
        if let Some(parent) = path.parent() {
            private_dir(parent)?;
        }
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path)?;
    }
    ensure!(
        fs::symlink_metadata(path)?.is_dir(),
        "State directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            fs::metadata(path)?.permissions().mode() & 0o022 == 0,
            "State directory must not be writable by other users"
        );
    }
    Ok(())
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "State file must not be a symlink"
    );
    let bytes = zeroize::Zeroizing::new(fs::read(path)?);
    serde_json::from_slice(&bytes)
        .map_err(|_| eyre::eyre!("Invalid saved JSON at {}", path.display()))
}

pub fn save_json(path: &Path, data: &impl Serialize, replace: bool) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| eyre::eyre!("Missing state directory"))?;
    private_dir(dir)?;
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec_pretty(data)?);
    if path.exists() {
        ensure!(fs::symlink_metadata(path)?.is_file(), "Invalid state file");
        if !replace {
            ensure!(
                *bytes == fs::read(path)?,
                "Refusing to replace a different saved note"
            );
            return Ok(());
        }
    }
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    if replace {
        temp.persist(path)?;
    } else {
        temp.persist_noclobber(path)?;
    }
    File::open(dir)?.sync_all()?;
    Ok(())
}

pub fn lock(directory: &Path) -> Result<File> {
    private_dir(directory)?;
    let path = directory.join("run.lock");
    if path.exists() {
        ensure!(fs::symlink_metadata(&path)?.is_file(), "Invalid lock file");
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    file.try_lock()
        .map_err(|_| eyre::eyre!("Another process is using {}", directory.display()))?;
    Ok(file)
}

#[derive(Serialize, Deserialize)]
pub struct SavedTx {
    pub raw: Bytes,
    pub hash: B256,
}

impl SavedTx {
    pub async fn receipt(&self, rpc: &impl Rpc) -> Result<Value> {
        ensure!(
            keccak256(&self.raw) == self.hash,
            "Saved transaction hash mismatch"
        );
        let hash = self.hash;
        let mut receipt = rpc
            .request("eth_getTransactionReceipt", json!([hash]))
            .await?;
        if receipt.is_null() {
            if rpc
                .request("eth_sendRawTransaction", json!([self.raw]))
                .await
                .is_err()
            {
                eprintln!("Broadcast not acknowledged; checking saved hash {hash:#x}");
            }
            receipt = tokio::time::timeout(Duration::from_secs(120), async {
                loop {
                    let result = rpc
                        .request("eth_getTransactionReceipt", json!([hash]))
                        .await?;
                    if !result.is_null() {
                        return Ok::<_, eyre::Report>(result);
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            })
            .await
            .map_err(|_| eyre::eyre!("Transaction {hash:#x} not confirmed; rerun to resume"))??;
        }
        ensure!(
            receipt["transactionHash"] == json!(hash),
            "Receipt transaction hash mismatch"
        );
        ensure!(
            receipt["status"] == "0x0" || receipt["status"] == "0x1",
            "Invalid receipt status"
        );
        Ok(receipt)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    version: u8,
    chain_id: String,
    owner: Address,
    gem_id: String,
    pub amount: String,
    transactions: BTreeMap<String, SavedTx>,
    #[serde(skip)]
    directory: PathBuf,
}
impl Journal {
    pub fn load(
        directory: &Path,
        owner: Address,
        gem_id: U256,
        chain: u64,
    ) -> Result<Option<Self>> {
        let path = directory.join("operation.json");
        if !path.exists() {
            return Ok(None);
        }
        let mut journal: Self = read_json(&path)?;
        ensure!(
            journal.version == 1
                && journal.owner == owner
                && journal.gem_id == gem_id.to_string()
                && journal.chain_id == chain.to_string(),
            "Journal does not match chain/wallet/GEM"
        );
        journal.directory = directory.to_owned();
        Ok(Some(journal))
    }
    pub fn new(
        directory: &Path,
        owner: Address,
        gem: U256,
        amount: U256,
        chain: u64,
    ) -> Result<Self> {
        let journal = Self {
            version: 1,
            owner,
            gem_id: gem.to_string(),
            amount: amount.to_string(),
            chain_id: chain.to_string(),
            transactions: BTreeMap::new(),
            directory: directory.to_owned(),
        };
        journal.save()?;
        Ok(journal)
    }
    fn save(&self) -> Result<()> {
        save_json(&self.directory.join("operation.json"), self, true)
    }
    pub fn has(&self, step: &str) -> bool {
        self.transactions.contains_key(step)
    }

    pub async fn resume(&mut self, rpc: &impl Rpc, step: &str) -> Result<Option<Value>> {
        let Some(tx) = self.transactions.get(step) else {
            return Ok(None);
        };
        let hash = tx.hash;
        println!("{step}: {hash:#x}");
        let receipt = tx.receipt(rpc).await?;
        if receipt["status"] == "0x0" {
            self.transactions.remove(step);
            self.save()?;
            eyre::bail!("{step} reverted ({hash:#x}); rerun to retry");
        }
        ensure!(receipt["status"] == "0x1", "Invalid receipt status");
        Ok(Some(receipt))
    }

    pub async fn send(
        &mut self,
        rpc: &impl Rpc,
        wallet: &Wallet,
        step: &str,
        to: Address,
        data: Vec<u8>,
    ) -> Result<Value> {
        if let Some(receipt) = self.resume(rpc, step).await? {
            return Ok(receipt);
        }
        let chain = self.chain_id.parse()?;
        let raw = wallet.prepare(rpc, to, data, chain).await?;
        self.transactions.insert(
            step.to_owned(),
            SavedTx {
                hash: keccak256(&raw),
                raw: raw.into(),
            },
        );
        self.save()?; // Publish signed bytes before broadcasting, including approvals/deposit.
        self.resume(rpc, step)
            .await?
            .ok_or_else(|| eyre::eyre!("Saved transaction missing"))
    }
}
