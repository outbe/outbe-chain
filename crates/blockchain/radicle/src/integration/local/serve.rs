//! Request frame routing and handler dispatch for the local receipt socket.

use super::*;
use tokio::sync::OwnedSemaphorePermit;

#[derive(Clone)]
pub(super) struct ServerState {
    pub(super) reader: Arc<dyn RepositoryReceiptReader>,
    pub(super) authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    pub(super) material: Option<EndpointNetworkResolver>,
    pub(super) publications: Option<Arc<Mutex<ReviewPackageStore>>>,
    pub(super) readers: Arc<Semaphore>,
    pub(super) native_node: Option<[u8; 32]>,
}

pub(super) async fn serve(mut stream: UnixStream, state: ServerState) -> Result<(), Error> {
    let request = read_frame(&mut stream, MAX_REQUEST).await?;
    let magic = classify_request(&request)?;
    let permit = match state.readers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return write_frame(&mut stream, &[magic.as_slice(), &[0]].concat()).await,
    };
    let response = dispatch(magic, request, state, permit)
        .await
        .unwrap_or_else(|_| [magic.as_slice(), &[0]].concat());
    write_frame(&mut stream, &response).await
}

fn classify_request(request: &[u8]) -> Result<&'static [u8; 4], Error> {
    let header = request.get(..4).ok_or(Error::Protocol)?;
    if let Some(magic) = classify_fixed_request(header, request.len()) {
        return Ok(magic);
    }
    classify_variable_request(header, request.len()).ok_or(Error::Protocol)
}

fn classify_fixed_request(header: &[u8], length: usize) -> Option<&'static [u8; 4]> {
    match length {
        4 => match header {
            magic if magic == SCOPE => Some(SCOPE),
            magic if magic == DESCRIPTOR => Some(DESCRIPTOR),
            magic if magic == gossip::TAKE => Some(gossip::TAKE),
            magic if magic == bootstrap::MAGIC => Some(bootstrap::MAGIC),
            magic if magic == binding::MAGIC => Some(binding::MAGIC),
            _ => None,
        },
        12 if header == MAGIC => Some(MAGIC),
        16 if header == vote::MAGIC => Some(vote::MAGIC),
        36 => match header {
            magic if magic == approval::MAGIC => Some(approval::MAGIC),
            magic if magic == bootstrap::SIGN_DOCUMENT => Some(bootstrap::SIGN_DOCUMENT),
            magic if magic == state::MAGIC => Some(state::MAGIC),
            magic if magic == material::MAGIC => Some(material::MAGIC),
            magic if magic == preparation::MAGIC => Some(preparation::MAGIC),
            _ => None,
        },
        _ if header == progress::MAGIC && length == 4 + PUBLICATION_BODY_LEN => {
            Some(progress::MAGIC)
        }
        _ => None,
    }
}

fn classify_variable_request(header: &[u8], length: usize) -> Option<&'static [u8; 4]> {
    match header {
        magic if magic == gossip::SEND && length > 4 => Some(gossip::SEND),
        magic if magic == publication::ATTEST && length > 4 => Some(publication::ATTEST),
        magic if magic == publication::QUEUE && length > 4 => Some(publication::QUEUE),
        magic if magic == bootstrap::SIGN_IDENTITY && length > 36 => Some(bootstrap::SIGN_IDENTITY),
        magic if magic == bootstrap::SIGN_REFS && length > 36 => Some(bootstrap::SIGN_REFS),
        magic if magic == preparation::CACHE && length > 44 => Some(preparation::CACHE),
        _ => classify_review_request(header, length),
    }
}

fn classify_review_request(header: &[u8], length: usize) -> Option<&'static [u8; 4]> {
    if (header == review::COB || header == review::MERGE_COB) && length >= review::MIN_REQUEST {
        return Some(if header == review::COB {
            review::COB
        } else {
            review::MERGE_COB
        });
    }
    if (header == review::REFS || header == review::MERGE_REFS)
        && length >= review::MIN_REQUEST + 269
    {
        return Some(if header == review::REFS {
            review::REFS
        } else {
            review::MERGE_REFS
        });
    }
    None
}

async fn dispatch(
    magic: &'static [u8; 4],
    request: Vec<u8>,
    state: ServerState,
    permit: OwnedSemaphorePermit,
) -> Result<Vec<u8>, Error> {
    if is_read_magic(magic) {
        return dispatch_read(magic, &request, &state, permit).await;
    }
    if is_action_magic(magic) {
        return dispatch_action(magic, &request, &state, permit).await;
    }
    dispatch_blocking(magic, request, state, permit).await
}

fn is_read_magic(magic: &'static [u8; 4]) -> bool {
    [
        DESCRIPTOR,
        vote::MAGIC,
        approval::MAGIC,
        binding::MAGIC,
        progress::MAGIC,
        gossip::SEND,
        gossip::TAKE,
        bootstrap::MAGIC,
        material::MAGIC,
    ]
    .contains(&magic)
}

fn is_action_magic(magic: &'static [u8; 4]) -> bool {
    [
        review::COB,
        review::REFS,
        review::MERGE_COB,
        review::MERGE_REFS,
        publication::ATTEST,
        publication::QUEUE,
        bootstrap::SIGN_IDENTITY,
        bootstrap::SIGN_REFS,
        bootstrap::SIGN_DOCUMENT,
        preparation::CACHE,
        preparation::MAGIC,
    ]
    .contains(&magic)
}

async fn dispatch_read(
    magic: &'static [u8; 4],
    request: &[u8],
    state: &ServerState,
    permit: OwnedSemaphorePermit,
) -> Result<Vec<u8>, Error> {
    match magic {
        magic if magic == DESCRIPTOR => read_descriptor(state.reader.clone(), permit).await,
        magic if magic == vote::MAGIC => read_vote(state.reader.clone(), permit, request).await,
        magic if magic == approval::MAGIC => {
            approval::read(
                state.reader.clone(),
                permit,
                U256::from_be_slice(&request[4..]),
            )
            .await
        }
        magic if magic == binding::MAGIC => {
            binding::read(state.authority.clone(), state.material.clone(), permit).await
        }
        magic if magic == progress::MAGIC => {
            read_progress(state.reader.clone(), permit, request).await
        }
        magic if magic == gossip::SEND || magic == gossip::TAKE => {
            gossip::relay(state.material.clone(), request).await
        }
        magic if magic == bootstrap::MAGIC => {
            bootstrap::read(state.authority.clone(), state.material.clone(), permit).await
        }
        _ => {
            material::read(
                state.authority.clone(),
                state.material.clone(),
                permit,
                U256::from_be_slice(&request[4..]),
            )
            .await
        }
    }
}

async fn dispatch_action(
    magic: &'static [u8; 4],
    request: &[u8],
    state: &ServerState,
    permit: OwnedSemaphorePermit,
) -> Result<Vec<u8>, Error> {
    match magic {
        magic
            if [
                review::COB,
                review::REFS,
                review::MERGE_COB,
                review::MERGE_REFS,
            ]
            .contains(&magic) =>
        {
            sign_review(state.clone(), permit, request).await
        }
        magic if magic == publication::ATTEST || magic == publication::QUEUE => {
            publish_package(state.clone(), permit, request).await
        }
        magic if magic == bootstrap::SIGN_IDENTITY || magic == bootstrap::SIGN_REFS => {
            bootstrap::sign_native(
                state.authority.clone(),
                state.material.clone(),
                state.readers.clone(),
                permit,
                request.to_vec(),
            )
            .await
        }
        magic if magic == bootstrap::SIGN_DOCUMENT => {
            bootstrap::sign_document(
                state.authority.clone(),
                state.material.clone(),
                permit,
                B256::from_slice(&request[4..]),
            )
            .await
        }
        magic if magic == preparation::CACHE => preparation::cache(
            state.reader.clone(),
            state.publications.clone(),
            permit,
            request.to_vec(),
        )
        .await
        .map(|()| [magic.as_slice(), &[2]].concat()),
        _ => {
            preparation::read(
                state.reader.clone(),
                state.authority.clone(),
                state.material.clone(),
                state.publications.clone(),
                state.readers.clone(),
                permit,
                U256::from_be_slice(&request[4..]),
            )
            .await
        }
    }
}

async fn read_descriptor(
    reader: Arc<dyn RepositoryReceiptReader>,
    permit: OwnedSemaphorePermit,
) -> Result<Vec<u8>, Error> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let descriptor = reader
            .read_governments_descriptor()
            .ok()
            .flatten()
            .ok_or(Error::Unavailable)?;
        let bytes = descriptor.to_bytes().map_err(|_| Error::Unavailable)?;
        Ok([DESCRIPTOR.as_slice(), &[2], &bytes].concat())
    })
    .await
    .map_err(|_| Error::Unavailable)?
}

async fn read_vote(
    reader: Arc<dyn RepositoryReceiptReader>,
    permit: OwnedSemaphorePermit,
    request: &[u8],
) -> Result<Vec<u8>, Error> {
    let height = u64::from_be_bytes(request[4..12].try_into().map_err(|_| Error::Protocol)?);
    let offset = u32::from_be_bytes(request[12..16].try_into().map_err(|_| Error::Protocol)?);
    vote::read(reader, permit, height, offset).await
}

async fn read_progress(
    reader: Arc<dyn RepositoryReceiptReader>,
    permit: OwnedSemaphorePermit,
    request: &[u8],
) -> Result<Vec<u8>, Error> {
    let body = PublicationBody::from_bytes(&request[4..]).map_err(|_| Error::Protocol)?;
    progress::read(reader, permit, body).await
}

async fn sign_review(
    state: ServerState,
    permit: OwnedSemaphorePermit,
    request: &[u8],
) -> Result<Vec<u8>, Error> {
    let parsed = review::Request::parse(request, state.native_node)?;
    review::sign(
        state.reader,
        state.authority,
        state.material,
        state.publications,
        state.readers,
        permit,
        parsed,
    )
    .await
}

async fn publish_package(
    state: ServerState,
    permit: OwnedSemaphorePermit,
    request: &[u8],
) -> Result<Vec<u8>, Error> {
    let parsed = publication::Request::parse(request, state.native_node)?;
    match parsed.body.action {
        outbe_primitives::radicle_publication::PublicationAction::Bootstrap => {
            publication::publish(
                state.authority,
                state.material,
                state.publications,
                state.readers,
                permit,
                parsed,
            )
            .await
        }
        outbe_primitives::radicle_publication::PublicationAction::Review
        | outbe_primitives::radicle_publication::PublicationAction::Merge => {
            review::publish(
                state.reader,
                state.authority,
                state.material,
                state.publications,
                state.readers,
                permit,
                parsed,
            )
            .await
        }
        outbe_primitives::radicle_publication::PublicationAction::BindMaterial => {
            binding::publish(
                state.reader,
                state.authority,
                state.material,
                state.publications,
                state.readers,
                permit,
                parsed,
            )
            .await
        }
    }
}

async fn dispatch_blocking(
    magic: &'static [u8; 4],
    request: Vec<u8>,
    state: ServerState,
    permit: OwnedSemaphorePermit,
) -> Result<Vec<u8>, Error> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if magic == SCOPE {
            let authority = state
                .authority
                .ok_or(Error::Unavailable)?
                .read_current_repository_authority()
                .map_err(|_| Error::Unavailable)?;
            Ok([
                SCOPE.as_slice(),
                &[2],
                &authority.chain_id().to_be_bytes(),
                authority.genesis_hash().as_slice(),
            ]
            .concat())
        } else if magic == state::MAGIC {
            let proposal_id = U256::from_be_slice(&request[4..]);
            let current = state
                .authority
                .ok_or(Error::Unavailable)?
                .read_current_patch_signing_state(proposal_id)
                .map_err(|_| Error::Unavailable)?;
            state::encode(&current)
        } else {
            let sequence =
                u64::from_be_bytes(request[4..].try_into().map_err(|_| Error::Protocol)?);
            let receipt = state
                .reader
                .read_current_publication_receipt(sequence)
                .map_err(|_| Error::Unavailable)?;
            Ok(encode_receipt(receipt.as_ref()))
        }
    })
    .await
    .map_err(|_| Error::Unavailable)?
}
