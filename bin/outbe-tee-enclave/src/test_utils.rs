//! Sequential Unix transport accept loop for mock and unit tests.

use std::os::unix::net::{UnixListener, UnixStream};

use outbe_tee::errors::TransportError;

use crate::{
    initialization::InitializationState, keys::EnclaveKeys, seal::EnclaveBootConfig,
    transport::SharedTributeOfferKey,
};

/// Borrow the state required by one sequential test server.
pub struct SequentialTestServerContext<'a> {
    pub keys: &'a EnclaveKeys,
    pub offer_key: &'a SharedTributeOfferKey,
    pub boot: &'a EnclaveBootConfig,
    pub initialization: &'a InitializationState,
}

/// Serve a fixed number of connections with the caller's resident key slot.
pub fn serve_sequential_unix_connections(
    listener: UnixListener,
    context: SequentialTestServerContext<'_>,
    connections: usize,
    serve: fn(
        UnixStream,
        &EnclaveKeys,
        &SharedTributeOfferKey,
        Option<&EnclaveBootConfig>,
        &InitializationState,
    ) -> Result<(), TransportError>,
) {
    for _ in 0..connections {
        let (stream, _) = listener.accept().unwrap();
        serve(
            stream,
            context.keys,
            context.offer_key,
            Some(context.boot),
            context.initialization,
        )
        .unwrap();
    }
}
