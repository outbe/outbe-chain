use super::super::types::ControlSession;
use super::*;

struct ReconciliationPass {
    sessions: Vec<ControlSession>,
    blocked: BTreeSet<[u8; 32]>,
    converged: bool,
}

impl Runtime {
    pub(super) async fn reconcile_control(
        &mut self,
        snapshot: &FinalizedSnapshot,
        desired: &BTreeMap<[u8; 32], VerifiedEndpoint>,
    ) -> ControlReconciliation {
        let Ok(sessions) = self.dependencies.control.sessions().await else {
            self.status.uds_failures += 1;
            return ControlReconciliation {
                sidecar_available: false,
                converged: false,
            };
        };
        let mut pass = ReconciliationPass {
            sessions,
            blocked: BTreeSet::new(),
            converged: true,
        };
        self.observe_outbound_sessions(&pass.sessions);
        self.disconnect_stale(desired, &mut pass).await;
        self.connect_desired(desired, &mut pass).await;
        self.seed_repositories(snapshot, &mut pass).await;
        self.probe_convergence(desired, pass.converged).await
    }

    fn observe_outbound_sessions(&mut self, sessions: &[ControlSession]) {
        for session in sessions
            .iter()
            .filter(|session| session.direction == SessionDirection::Outbound)
        {
            self.managed
                .insert(session.node_id, session.address.clone());
        }
    }
    async fn disconnect_stale(
        &mut self,
        desired: &BTreeMap<[u8; 32], VerifiedEndpoint>,
        pass: &mut ReconciliationPass,
    ) {
        let stale = self
            .managed
            .iter()
            .filter_map(|(node_id, address)| {
                let keep = desired
                    .get(node_id)
                    .is_some_and(|peer| peer.addresses.contains(address));
                (!keep).then_some((*node_id, address.clone()))
            })
            .collect::<Vec<_>>();
        for (node_id, address) in stale {
            match self
                .dependencies
                .control
                .disconnect(node_id, &address)
                .await
            {
                Ok(DisconnectDisposition::Disconnected | DisconnectDisposition::AlreadyAbsent) => {
                    self.managed.remove(&node_id);
                    pass.sessions.retain(|session| {
                        session.node_id != node_id
                            || session.direction != SessionDirection::Outbound
                            || session.address != address
                    });
                }
                Ok(DisconnectDisposition::Inbound) => {
                    self.managed.remove(&node_id);
                }
                Ok(DisconnectDisposition::NotConnected | DisconnectDisposition::AddressChanged)
                | Err(_) => {
                    pass.blocked.insert(node_id);
                    pass.converged = false;
                }
            }
        }
    }
    async fn connect_desired(
        &mut self,
        desired: &BTreeMap<[u8; 32], VerifiedEndpoint>,
        pass: &mut ReconciliationPass,
    ) {
        for (node_id, endpoint) in desired {
            if pass.blocked.contains(node_id) {
                continue;
            }
            if pass.connected(*node_id, endpoint) {
                if let Some(address) = pass.managed_outbound_address(*node_id, endpoint) {
                    self.managed.insert(*node_id, address.clone());
                }
                continue;
            }
            match self
                .dependencies
                .control
                .connect(*node_id, &endpoint.addresses)
                .await
            {
                Ok(address) => {
                    self.managed.insert(*node_id, address);
                }
                Err(_) => {
                    pass.converged = false;
                }
            }
        }
    }
    async fn seed_repositories(
        &mut self,
        snapshot: &FinalizedSnapshot,
        pass: &mut ReconciliationPass,
    ) {
        for repo in &snapshot.repositories {
            if self.dependencies.control.seed(*repo).await.is_err() {
                pass.converged = false;
            }
        }
    }
    async fn probe_convergence(
        &mut self,
        desired: &BTreeMap<[u8; 32], VerifiedEndpoint>,
        converged: bool,
    ) -> ControlReconciliation {
        match self.dependencies.control.sessions().await {
            Ok(sessions) => {
                self.status.connected_peer_count = sessions
                    .into_iter()
                    .filter(|session| session.connected && desired.contains_key(&session.node_id))
                    .map(|session| session.node_id)
                    .collect::<BTreeSet<_>>()
                    .len();
                ControlReconciliation {
                    sidecar_available: true,
                    converged,
                }
            }
            Err(_) => {
                self.status.uds_failures += 1;
                self.status.connected_peer_count = 0;
                ControlReconciliation {
                    sidecar_available: false,
                    converged: false,
                }
            }
        }
    }
}

impl ReconciliationPass {
    fn connected(&self, node_id: [u8; 32], endpoint: &VerifiedEndpoint) -> bool {
        self.sessions.iter().any(|session| {
            session.node_id == node_id
                && session.connected
                && (session.direction == SessionDirection::Inbound
                    || endpoint.addresses.contains(&session.address))
        })
    }
    fn managed_outbound_address(
        &self,
        node_id: [u8; 32],
        endpoint: &VerifiedEndpoint,
    ) -> Option<&EndpointAddress> {
        self.sessions
            .iter()
            .find(|session| {
                session.node_id == node_id
                    && session.connected
                    && session.direction == SessionDirection::Outbound
                    && endpoint.addresses.contains(&session.address)
            })
            .map(|session| &session.address)
    }
}
