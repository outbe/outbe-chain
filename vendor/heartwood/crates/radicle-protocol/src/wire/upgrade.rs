//! Repository-key negotiation belongs to one authenticated Noise connection.

use radicle::node::Features;

use super::{Invalid, frame::Control};

#[derive(Debug, Default)]
pub struct Negotiation {
    local: Option<bool>,
    peer: Option<bool>,
    requested: bool,
    acknowledged: bool,
    peer_acknowledged: bool,
}

impl Negotiation {
    /// Call only after sending our own signed node announcement.
    pub fn sent_announcement(&mut self, features: Features) -> Result<Option<Control>, Invalid> {
        let capable = features.has(Features::REPOSITORY_KEYS);
        if self.requested && !capable {
            return Err(Invalid::RepositoryKeyNegotiation);
        }
        self.local = Some(capable);
        Ok(self.request())
    }

    /// Call only for a verified announcement signed by the connected peer itself.
    pub fn authenticated_announcement(
        &mut self,
        features: Features,
    ) -> Result<Option<Control>, Invalid> {
        let capable = features.has(Features::REPOSITORY_KEYS);
        if self.requested && !capable {
            return Err(Invalid::RepositoryKeyNegotiation);
        }
        self.peer = Some(capable);
        Ok(self.request())
    }

    fn capable(&self) -> bool {
        self.local == Some(true) && self.peer == Some(true)
    }

    fn request(&mut self) -> Option<Control> {
        if self.capable() && !self.requested {
            self.requested = true;
            Some(Control::Upgrade { version: 2 })
        } else {
            None
        }
    }

    /// The returned Ack must be queued on this connection before accepting its V2 frames.
    pub fn control(&mut self, control: &Control) -> Result<Option<Control>, Invalid> {
        if !self.capable() {
            return Err(Invalid::RepositoryKeyNegotiation);
        }
        match control {
            Control::Upgrade { version: 2 } => {
                self.peer_acknowledged = true;
                Ok(Some(Control::UpgradeAck { version: 2 }))
            }
            Control::UpgradeAck { version: 2 } if self.requested => {
                self.acknowledged = true;
                Ok(None)
            }
            _ => Err(Invalid::RepositoryKeyNegotiation),
        }
    }

    pub fn receive(&self, version: u8) -> Result<(), Invalid> {
        match version {
            1 => Ok(()),
            2 if self.peer_acknowledged => Ok(()),
            _ => Err(Invalid::RepositoryKeyNegotiation),
        }
    }

    pub fn send(&self, version: u8) -> Result<(), Invalid> {
        match version {
            1 => Ok(()),
            2 if self.acknowledged => Ok(()),
            _ => Err(Invalid::UnsupportedRepositoryKey),
        }
    }

    pub fn peer_unsupported(&self) -> bool {
        self.peer == Some(false) || self.local == Some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_directions_and_connection_reset() {
        let mut a = Negotiation::default();
        let mut b = Negotiation::default();
        assert!(a.send(2).is_err());
        assert!(a.receive(2).is_err());
        assert!(a.control(&Control::Upgrade { version: 2 }).is_err());
        assert!(
            a.sent_announcement(Features::REPOSITORY_KEYS)
                .unwrap()
                .is_none()
        );
        assert!(
            b.authenticated_announcement(Features::REPOSITORY_KEYS)
                .unwrap()
                .is_none()
        );
        let a_request = a
            .authenticated_announcement(Features::REPOSITORY_KEYS)
            .unwrap()
            .unwrap();
        let b_request = b
            .sent_announcement(Features::REPOSITORY_KEYS)
            .unwrap()
            .unwrap();
        let b_ack = b.control(&a_request).unwrap().unwrap();
        assert!(b.receive(2).is_ok());
        assert!(b.send(2).is_err());
        a.control(&b_ack).unwrap();
        assert!(a.send(2).is_ok());
        assert!(a.receive(2).is_err());
        let a_ack = a.control(&b_request).unwrap().unwrap();
        b.control(&a_ack).unwrap();
        assert!(a.receive(2).is_ok());
        assert!(b.send(2).is_ok());
        assert!(a.control(&b_request).unwrap().is_some());
        a.control(&b_ack).unwrap();
        assert!(a.send(1).is_ok());
        assert!(a.receive(1).is_ok());
        assert!(a.authenticated_announcement(Features::SEED).is_err());
        let fresh = Negotiation::default();
        assert!(fresh.send(2).is_err());
        assert!(fresh.receive(2).is_err());
    }

    #[test]
    fn both_capabilities_and_exact_version_are_required() {
        let mut n = Negotiation::default();
        n.sent_announcement(Features::REPOSITORY_KEYS).unwrap();
        assert!(
            n.authenticated_announcement(Features::SEED)
                .unwrap()
                .is_none()
        );
        assert!(n.peer_unsupported());
        assert!(n.control(&Control::UpgradeAck { version: 2 }).is_err());
        n.authenticated_announcement(Features::REPOSITORY_KEYS)
            .unwrap();
        assert!(n.control(&Control::Upgrade { version: 3 }).is_err());
        assert!(n.control(&Control::UpgradeAck { version: 1 }).is_err());
        assert!(n.receive(3).is_err());
    }
}
