//! Handle remote sessions requests after session admission.

use super::requests::RequestContext;
use crate::transport::*;

pub(super) fn dispatch(req: EnclaveRequest, context: RequestContext<'_>) -> EnclaveResponse {
    let RequestContext {
        keys,
        initialization: context,
        ..
    } = context;
    let initialization = context.initialization;
    match req {
        EnclaveRequest::AuthorizeRemoteSessionV2 {
            ticket_id,
            initiator_static_x25519,
            responder_static_x25519,
            deadline,
            finalized_block_hash,
            retirement_height,
        } => {
            let result = initialization
                .ok_or_else(|| "remote admission requires initialization".to_string())
                .and_then(|state| {
                    state.retire_remote_sessions(retirement_height)?;
                    state.authorize_remote_session_at_generation(
                        crate::initialization::RemoteSessionAuthorization {
                            ticket_id,
                            initiator_static_x25519,
                            responder_static_x25519,
                            deadline,
                            finalized_block_hash,
                        },
                        retirement_height,
                        keys,
                    )
                });
            match result {
                Ok(()) => EnclaveResponse::RemoteSessionAuthorizedV1 { ticket_id },
                Err(message) => EnclaveResponse::Error { message },
            }
        }
        EnclaveRequest::AuthorizeRemoteSessionV1 {
            ticket_id,
            initiator_static_x25519,
            responder_static_x25519,
            deadline,
            finalized_block_hash,
        } => {
            let Some(initialization) = initialization else {
                return EnclaveResponse::Error {
                    message: "remote session authorization requires production initialization"
                        .into(),
                };
            };
            match initialization.authorize_remote_session(
                crate::initialization::RemoteSessionAuthorization {
                    ticket_id,
                    initiator_static_x25519,
                    responder_static_x25519,
                    deadline,
                    finalized_block_hash,
                },
                keys,
            ) {
                Ok(()) => EnclaveResponse::RemoteSessionAuthorizedV1 { ticket_id },
                Err(message) => EnclaveResponse::Error { message },
            }
        }
        EnclaveRequest::RetireRemoteSessionsV1 { activation_height } => {
            match initialization
                .ok_or_else(|| "remote retirement requires initialized enclave".to_string())
                .and_then(|state| state.retire_remote_sessions(activation_height))
            {
                Ok(()) => EnclaveResponse::RemoteSessionsRetiredV1 { activation_height },
                Err(message) => EnclaveResponse::Error { message },
            }
        }
        _ => unreachable!("request family is checked by the exhaustive dispatcher"),
    }
}
