use super::*;
use crate::endpoint::EndpointRequest;

pub(super) struct EndpointReceiveContext<'a, S> {
    pub(super) sender: &'a mut S,
    pub(super) signer: &'a bls12381::PrivateKey,
    pub(super) local: &'a LocalEndpointIdentityHandle,
    pub(super) snapshot: Option<&'a FinalizedSnapshot>,
    pub(super) queued: &'a mut BTreeMap<PeerId, (SignedEndpointResponse, u64)>,
}

impl EndpointNetworkState {
    pub(super) async fn receive<S>(
        &self,
        context: EndpointReceiveContext<'_, S>,
        received: (bls12381::PublicKey, IoBuf),
    ) -> Result<(), ManagerError>
    where
        S: LimitedSender<PublicKey = bls12381::PublicKey>,
    {
        let (sender_key, bytes) = received;
        let peer = PeerId::from_public_key(&sender_key);
        match EndpointFrame::decode(bytes.as_ref()) {
            Ok(EndpointFrame::Request(request)) => self.respond_to_request(context, peer, request),
            Ok(EndpointFrame::Response(response)) => {
                self.receive_response(context, peer, *response).await
            }
            Err(_) => Ok(()),
        }
    }
    fn respond_to_request<S>(
        &self,
        context: EndpointReceiveContext<'_, S>,
        peer: PeerId,
        request: EndpointRequest,
    ) -> Result<(), ManagerError>
    where
        S: LimitedSender<PublicKey = bls12381::PublicKey>,
    {
        let EndpointReceiveContext {
            sender,
            signer,
            local,
            snapshot,
            ..
        } = context;

        if self.status.snapshot().voting_gate != RadicleVotingGate::SignerAllowed {
            return Ok(());
        }
        let Some(local) = local.current() else {
            return Ok(());
        };
        let Some(snapshot) = snapshot else {
            return Ok(());
        };
        let Some(validator) = snapshot.validators.iter().find(|validator| {
            validator.address == local.validator
                && validator.peer == PeerId::from_public_key(&signer.public_key())
                && validator.node_id == Some(local.node_id)
        }) else {
            return Ok(());
        };
        let Some(valid_until) = snapshot.block.number.checked_add(MAX_ENDPOINT_TTL_BLOCKS) else {
            return Ok(());
        };
        let response = sign_response(
            EndpointResponseBody {
                request_id: request.request_id(),
                chain_id: self.chain.chain_id,
                genesis_hash: self.chain.genesis_hash,
                validator: validator.address,
                node_id: local.node_id,
                addresses: local.addresses.clone(),
                anchor_number: snapshot.block.number,
                anchor_hash: snapshot.block.hash,
                valid_until,
            },
            signer,
        )
        .map_err(|error| ManagerError::Endpoint(error.to_string()))?;
        let _ = send(
            sender,
            peer,
            EndpointFrame::Response(Box::new(response)).encode(),
        );
        Ok(())
    }
    async fn receive_response<S>(
        &self,
        context: EndpointReceiveContext<'_, S>,
        peer: PeerId,
        response: SignedEndpointResponse,
    ) -> Result<(), ManagerError> {
        let EndpointReceiveContext {
            snapshot, queued, ..
        } = context;

        let Some(snapshot) = snapshot else {
            return Ok(());
        };
        let anchor = (response.body().anchor_number == snapshot.block.number)
            .then(|| anchor(snapshot))
            .transpose()?;
        match self
            .handle
            .response(
                peer,
                response.clone(),
                snapshot.block.number,
                anchor,
                now_millis(),
            )
            .await
        {
            Ok(ReceiveOutcome::Verified(verified)) => {
                self.publish(peer, response, verified);
            }
            Ok(ReceiveOutcome::Queued { .. }) => {
                queued.insert(
                    peer,
                    (
                        response,
                        now_millis().saturating_add(UNKNOWN_ANCHOR_TIMEOUT_MS),
                    ),
                );
            }
            Err(_) => {}
        }
        Ok(())
    }
}
