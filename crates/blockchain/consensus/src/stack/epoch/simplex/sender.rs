//! Drop outbound votes/certificates during role drain without interrupting voter WAL writes.
use super::proposal::Permission;
use commonware_actor::{Feedback, Unreliable};
use commonware_p2p::{CheckedSender, LimitedSender, Recipients};
use commonware_runtime::IoBufs;
use std::time::SystemTime;

#[derive(Clone)]
pub(super) struct VoteSenderGate<S> {
    inner: S,
    permission: Permission,
}
impl<S> VoteSenderGate<S> {
    pub(super) fn new(inner: S, permission: Permission) -> Self {
        Self { inner, permission }
    }
}
pub(super) struct CheckedGate<S> {
    inner: Option<S>,
    permission: Permission,
}
impl<S: LimitedSender> LimitedSender for VoteSenderGate<S> {
    type PublicKey = S::PublicKey;
    type Checked<'a>
        = CheckedGate<S::Checked<'a>>
    where
        Self: 'a;
    fn check(
        &mut self,
        recipients: Recipients<Self::PublicKey>,
    ) -> Result<Self::Checked<'_>, SystemTime> {
        let inner = if (self.permission)() {
            Some(self.inner.check(recipients)?)
        } else {
            None
        };
        Ok(CheckedGate {
            inner,
            permission: self.permission.clone(),
        })
    }
}
impl<S: CheckedSender> CheckedSender for CheckedGate<S> {
    type PublicKey = S::PublicKey;
    fn recipients(&self) -> Vec<Self::PublicKey> {
        if !(self.permission)() {
            return Vec::new();
        }
        self.inner
            .as_ref()
            .map_or_else(Vec::new, CheckedSender::recipients)
    }
    fn send(self, message: impl Into<IoBufs> + Send, priority: bool) -> Unreliable<Feedback> {
        if !(self.permission)() {
            return Unreliable::Rejected;
        }
        match self.inner {
            Some(inner) => inner.send(message, priority),
            None => Unreliable::Rejected,
        }
    }
}
