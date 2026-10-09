use super::*;

pub(super) enum Response {
    Immediate(EnclaveResponse),
    Admitted(EnclaveResponse),
}

pub(super) struct CommandSession {
    dkg: DkgSessionStore,
    dcap_verification: DcapVerificationSessionV1,
    onboarding_upload: OnboardingArtifactUploadSessionV1,
}

impl CommandSession {
    pub(super) fn new() -> Self {
        Self {
            dkg: DkgSessionStore::new(),
            dcap_verification: DcapVerificationSessionV1::default(),
            onboarding_upload: OnboardingArtifactUploadSessionV1::default(),
        }
    }

    pub(super) fn prepare_response(
        &mut self,
        req: EnclaveRequest,
        context: ConnectionContext<'_>,
        session_authority: SessionAuthorityV1,
        peer: &'static str,
    ) -> Response {
        let ConnectionContext {
            initialization,
            offer_key,
            ..
        } = context;
        let req_label = req.label();
        let req_class = crate::initialization::request_class_label(&req);
        let req_started = std::time::SystemTime::now();
        let is_onboarding_upload_request = matches!(
            req,
            EnclaveRequest::BeginUpgradeKeyTransferV1 { .. }
                | EnclaveRequest::BeginDcapOnboardingArtifactIngestV1 { .. }
                | EnclaveRequest::DcapOnboardingArtifactChunkV1 { .. }
                | EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 { .. }
                | EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { .. }
        );
        if self.onboarding_upload.is_active() && !is_onboarding_upload_request {
            self.onboarding_upload.abort();
            let response = EnclaveResponse::Error {
                message: "onboarding artifact upload cannot be interleaved with another command"
                    .into(),
            };
            return Response::Immediate(response);
        }
        if let Err(denial) =
            initialization.authorize_command(&req, offer_key.get().is_some(), session_authority)
        {
            if is_onboarding_upload_request {
                self.onboarding_upload.abort();
            }
            let (ts, dur_ms) = crate::telemetry::now_unix_and_elapsed_ms(req_started);
            crate::telemetry::record_request(req_class, crate::telemetry::RequestOutcome::Denied);
            eprintln!(
                "{}",
                crate::telemetry::format_request_log(
                    ts,
                    req_label,
                    peer,
                    crate::telemetry::RequestOutcome::Denied,
                    dur_ms,
                )
            );
            let response = match denial {
                crate::initialization::CommandDenial::NotReady(message) => {
                    EnclaveResponse::NotReady {
                        message: message.into(),
                    }
                }
                crate::initialization::CommandDenial::Forbidden(message) => {
                    EnclaveResponse::Error {
                        message: message.into(),
                    }
                }
            };
            return Response::Immediate(response);
        }

        let resp = self.dispatch(req, context);

        let outcome = if matches!(resp, EnclaveResponse::Error { .. }) {
            crate::telemetry::RequestOutcome::Err
        } else {
            crate::telemetry::RequestOutcome::Ok
        };
        let (ts, dur_ms) = crate::telemetry::now_unix_and_elapsed_ms(req_started);
        crate::telemetry::record_request(req_class, outcome);
        eprintln!(
            "{}",
            crate::telemetry::format_request_log(ts, req_label, peer, outcome, dur_ms)
        );

        Response::Admitted(resp)
    }

    fn dispatch(&mut self, req: EnclaveRequest, context: ConnectionContext<'_>) -> EnclaveResponse {
        let ConnectionContext {
            keys,
            offer_key,
            boot,
            initialization,
            chain_id,
            quote_generator,
        } = context;
        match req {
            EnclaveRequest::PrepareGramineDirectDevOnboardingArtifactV1 {
                request_hash,
                context,
            } => match initialization.manifest() {
                Ok(manifest) => complete_gramine_direct_dev_onboarding_response(
                    request_hash,
                    &context,
                    offer_key.get(),
                    manifest.as_ref(),
                ),
                Err(message) => EnclaveResponse::Error { message },
            },
            request @ (EnclaveRequest::BeginDcapVerificationV1 { .. }
            | EnclaveRequest::BeginDcapOnboardingVerificationV1 { .. }
            | EnclaveRequest::DcapVerificationChunkV1 { .. }
            | EnclaveRequest::FinishDcapVerificationV1 { .. }) => {
                self.handle_verification(request, context)
            }
            request @ (EnclaveRequest::BeginUpgradeKeyTransferV1 { .. }
            | EnclaveRequest::BeginDcapOnboardingArtifactIngestV1 { .. }
            | EnclaveRequest::DcapOnboardingArtifactChunkV1 { .. }
            | EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 { .. }
            | EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { .. }) => {
                self.handle_upload(request, context)
            }
            request => dispatch_with_initialization(
                request,
                keys,
                &mut self.dkg,
                offer_key,
                DispatchInitializationContext {
                    chain_id,
                    boot,
                    initialization: Some(initialization),
                    quote_generator,
                },
            ),
        }
    }

    fn handle_verification(
        &mut self,
        request: EnclaveRequest,
        context: ConnectionContext<'_>,
    ) -> EnclaveResponse {
        let ConnectionContext {
            keys,
            offer_key,
            initialization,
            ..
        } = context;
        if initialization.mode() != InitializationMode::Production {
            EnclaveResponse::Error {
                message: "DCAP verification requires initialized production state".to_string(),
            }
        } else {
            match self.dcap_verification.handle(request) {
                Ok(DcapVerificationProgressV1::Started { request_hash }) => {
                    EnclaveResponse::DcapVerificationStartedV1 { request_hash }
                }
                Ok(DcapVerificationProgressV1::ChunkAccepted {
                    request_hash,
                    next_offset,
                }) => EnclaveResponse::DcapVerificationChunkAcceptedV1 {
                    request_hash,
                    next_offset,
                },
                Ok(DcapVerificationProgressV1::Complete(request)) => {
                    match initialization.manifest() {
                        Ok(manifest) => complete_verification_response(
                            *request,
                            keys,
                            offer_key.get(),
                            manifest.as_ref(),
                        ),
                        Err(message) => EnclaveResponse::Error { message },
                    }
                }
                Err(message) => EnclaveResponse::Error {
                    message: message.to_string(),
                },
            }
        }
    }

    fn handle_upload(
        &mut self,
        request: EnclaveRequest,
        context: ConnectionContext<'_>,
    ) -> EnclaveResponse {
        let ConnectionContext {
            keys,
            offer_key,
            boot,
            initialization,
            ..
        } = context;
        match self
            .onboarding_upload
            .handle(request, initialization.trusted_network_descriptor())
        {
            Ok(OnboardingArtifactUploadProgressV1::Started { request_hash }) => {
                EnclaveResponse::DcapOnboardingArtifactIngestStartedV1 { request_hash }
            }
            Ok(OnboardingArtifactUploadProgressV1::ChunkAccepted {
                request_hash,
                next_offset,
            }) => EnclaveResponse::DcapOnboardingArtifactChunkAcceptedV1 {
                request_hash,
                next_offset,
            },
            Ok(OnboardingArtifactUploadProgressV1::RecordAccepted { request_hash, kind }) => {
                EnclaveResponse::DcapOnboardingArtifactRecordAcceptedV1 { request_hash, kind }
            }
            Ok(OnboardingArtifactUploadProgressV1::Complete(complete)) => {
                complete_onboarding_artifact_ingest_response(
                    *complete,
                    keys,
                    offer_key,
                    boot,
                    initialization,
                )
            }
            Err(message) => EnclaveResponse::Error {
                message: message.to_string(),
            },
        }
    }
}
