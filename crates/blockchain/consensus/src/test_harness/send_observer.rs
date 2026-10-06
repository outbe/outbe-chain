//! Recipient selection shared by test senders with distinct message observations.

use commonware_actor::{Feedback, Unreliable};
use commonware_cryptography::PublicKey;
use commonware_p2p::{CheckedSender, LimitedSender, Recipients};
use commonware_runtime::IoBufs;
use std::time::SystemTime;

pub trait SendObserver: Clone + Send + Sync + 'static {
    fn on_send(self, message: impl Into<IoBufs> + Send);
}

#[derive(Clone)]
pub struct ObservedSender<P, O> {
    pub participants: Vec<P>,
    pub observer: Option<O>,
}

pub struct ObservedCheckedSender<P, O> {
    recipients: Vec<P>,
    observer: Option<O>,
}

impl<P: PublicKey, O: SendObserver> CheckedSender for ObservedCheckedSender<P, O> {
    type PublicKey = P;

    fn recipients(&self) -> Vec<P> {
        self.recipients.clone()
    }

    fn send(self, message: impl Into<IoBufs> + Send, _priority: bool) -> Unreliable<Feedback> {
        if let Some(observer) = self.observer {
            observer.on_send(message);
        }
        Unreliable::Outcome(Feedback::Ok)
    }
}

impl<P: PublicKey, O: SendObserver> LimitedSender for ObservedSender<P, O> {
    type PublicKey = P;
    type Checked<'a>
        = ObservedCheckedSender<P, O>
    where
        Self: 'a;

    fn check(&mut self, recipients: Recipients<P>) -> Result<Self::Checked<'_>, SystemTime> {
        let recipients = match recipients {
            Recipients::All => self.participants.clone(),
            Recipients::Some(recipients) => recipients,
            Recipients::One(recipient) => vec![recipient],
        };
        Ok(ObservedCheckedSender {
            recipients,
            observer: self.observer.clone(),
        })
    }
}
