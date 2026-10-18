//! Bounded delivery of the node's existing finalized Vote events.
use super::*;
use crate::manager::{FinalizedVotePage, MAX_VOTE_PAGE};
use tokio::sync::OwnedSemaphorePermit;

pub(super) const MAGIC: &[u8; 4] = b"OVF1";
const HEADER: usize = 5 + 8 + 32 + 8 + 32 + 4 + 1;
const MAX_FRAME: usize = HEADER + MAX_VOTE_PAGE * 32;

impl LocalReceiptClient {
    pub async fn read_finalized_vote_page(
        &self,
        height: u64,
        offset: u32,
    ) -> Result<Option<FinalizedVotePage>, Error> {
        let request = [
            MAGIC.as_slice(),
            &height.to_be_bytes(),
            &offset.to_be_bytes(),
        ]
        .concat();
        decode(
            &self.exchange(&request, MAX_FRAME).await?,
            height,
            offset,
            self.chain,
        )
    }
}

fn validate(page: &FinalizedVotePage, height: u64, offset: u32) -> Result<(), Error> {
    let invalid_block_or_len = page.block.number != height
        || page.block.hash.is_zero()
        || page.proposals.len() > MAX_VOTE_PAGE;
    let invalid_proposals_or_offset = page.proposals.iter().any(|id| id.is_zero())
        || page.proposals.windows(2).any(|p| p[0] >= p[1])
        || page
            .next_offset
            .is_some_and(|n| offset.checked_add(MAX_VOTE_PAGE as u32) != Some(n));
    if invalid_block_or_len || invalid_proposals_or_offset {
        return Err(Error::Protocol);
    }
    Ok(())
}

pub(super) async fn read(
    reader: Arc<dyn RepositoryReceiptReader>,
    permit: OwnedSemaphorePermit,
    height: u64,
    offset: u32,
) -> Result<Vec<u8>, Error> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let Some(page) = reader
            .read_finalized_vote_page(height, offset)
            .map_err(|_| Error::Unavailable)?
        else {
            return Ok([MAGIC.as_slice(), &[1]].concat());
        };
        validate(&page, height, offset)?;
        let mut bytes = [
            MAGIC.as_slice(),
            &[2],
            &page.chain_id.to_be_bytes(),
            page.genesis_hash.as_slice(),
            &page.block.number.to_be_bytes(),
            page.block.hash.as_slice(),
            &page.next_offset.unwrap_or(u32::MAX).to_be_bytes(),
            &[page.proposals.len() as u8],
        ]
        .concat();
        for id in page.proposals {
            bytes.extend(id.to_be_bytes::<32>());
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| Error::Unavailable)?
}

fn decode(
    bytes: &[u8],
    height: u64,
    offset: u32,
    chain: ChainIdentity,
) -> Result<Option<FinalizedVotePage>, Error> {
    if bytes == [MAGIC.as_slice(), &[0]].concat() {
        return Err(Error::Unavailable);
    }
    if bytes == [MAGIC.as_slice(), &[1]].concat() {
        return Ok(None);
    }
    if bytes.len() < HEADER
        || bytes.len() > MAX_FRAME
        || bytes.get(..5) != Some([MAGIC.as_slice(), &[2]].concat().as_slice())
    {
        return Err(Error::Protocol);
    }
    let mut input = state::Input(&bytes[5..]);
    let chain_id = u64::from_be_bytes(input.array()?);
    let genesis_hash = B256::from(input.array::<32>()?);
    let number = u64::from_be_bytes(input.array()?);
    let hash = B256::from(input.array::<32>()?);
    let next = u32::from_be_bytes(input.array()?);
    let count = input.take(1)?[0] as usize;
    if chain_id != chain.chain_id || genesis_hash != chain.genesis_hash || count > MAX_VOTE_PAGE {
        return Err(Error::Protocol);
    }
    let proposals = (0..count)
        .map(|_| input.array().map(U256::from_be_bytes::<32>))
        .collect::<Result<_, _>>()?;
    if !input.0.is_empty() {
        return Err(Error::Protocol);
    }
    let page = FinalizedVotePage {
        chain_id,
        genesis_hash,
        block: FinalizedBlock { number, hash },
        next_offset: (next != u32::MAX).then_some(next),
        proposals,
    };
    validate(&page, height, offset)?;
    Ok(Some(page))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Reader(Mutex<FinalizedVotePage>);
    impl RepositoryReceiptReader for Reader {
        fn read_current_publication_receipt(
            &self,
            _: u64,
        ) -> Result<Option<FinalizedPublicationReceipt>, crate::manager::ManagerError> {
            Ok(None)
        }
        fn read_finalized_vote_page(
            &self,
            height: u64,
            _: u32,
        ) -> Result<Option<FinalizedVotePage>, crate::manager::ManagerError> {
            Ok((height == 42).then(|| self.0.lock().unwrap().clone()))
        }
    }
    #[tokio::test]
    async fn existing_vote_events_cross_private_socket_with_chain_page_and_size_checks() {
        let directory = tempfile::tempdir_in(if cfg!(target_os = "macos") {
            "/private/tmp"
        } else {
            "/tmp"
        })
        .unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let control = directory.path().join("control");
        let chain = ChainIdentity {
            chain_id: 2026,
            genesis_hash: B256::repeat_byte(7),
        };
        let page = FinalizedVotePage {
            chain_id: chain.chain_id,
            genesis_hash: chain.genesis_hash,
            block: FinalizedBlock {
                number: 42,
                hash: B256::repeat_byte(42),
            },
            next_offset: Some(32),
            proposals: (1..=32).map(U256::from).collect(),
        };
        let reader = Arc::new(Reader(Mutex::new(page.clone())));
        let server = LocalReceiptServer::bind(&control, reader.clone()).unwrap();
        let (stop, stopping) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run(async {
            let _ = stopping.await;
        }));
        let client = LocalReceiptClient::new(&control, chain);
        assert_eq!(
            client.read_finalized_vote_page(42, 0).await.unwrap(),
            Some(page.clone())
        );
        assert!(client
            .read_finalized_vote_page(43, 0)
            .await
            .unwrap()
            .is_none());
        assert!(client.read_finalized_vote_page(42, 32).await.is_err());
        let foreign = ChainIdentity {
            genesis_hash: B256::repeat_byte(99),
            ..chain
        };
        assert!(LocalReceiptClient::new(&control, foreign)
            .read_finalized_vote_page(42, 0)
            .await
            .is_err());
        reader.0.lock().unwrap().proposals.push(U256::from(33));
        assert!(client.read_finalized_vote_page(42, 0).await.is_err());
        *reader.0.lock().unwrap() = page.clone();
        reader.0.lock().unwrap().proposals[1] = U256::ONE;
        assert!(client.read_finalized_vote_page(42, 0).await.is_err());
        *reader.0.lock().unwrap() = page.clone();
        let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        let mut bytes = read(reader.clone(), permit, 42, 0).await.unwrap();
        assert_eq!(bytes.len(), MAX_FRAME);
        bytes.push(0);
        assert!(decode(&bytes, 42, 0, chain).is_err());
        *reader.0.lock().unwrap() = FinalizedVotePage {
            next_offset: None,
            proposals: vec![],
            ..page
        };
        assert!(client
            .read_finalized_vote_page(42, 32)
            .await
            .unwrap()
            .unwrap()
            .proposals
            .is_empty());
        assert!(client.exchange(MAGIC, MAX_FRAME).await.is_err());
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    }
}
