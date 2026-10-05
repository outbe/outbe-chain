//! Zero-state resolver for marshal block-availability fixtures.

use commonware_actor::Feedback;
use commonware_consensus::marshal::resolver::handler;
use commonware_cryptography::bls12381;
use commonware_resolver::{Resolver, TargetedResolver};
use commonware_utils::vec::NonEmptyVec;

use crate::digest::Digest;

/// Accept requests without delivering blocks; the caller retains the handler keepalive.
#[derive(Clone, Default)]
pub struct NoopMarshalResolver;

// commonware 2026.5.0 split the resolver surface: the base `Resolver` keeps
// `fetch`/`fetch_all`/`retain` (now SYNC, returning `Feedback`, generic over
// `Into<Fetch<Key, Subscriber>>`) and gained `type Subscriber`; `cancel`/`clear`
// were removed; the targeted methods moved to `TargetedResolver`. The marshal
// actor requires `Key = handler::Key<Commitment>` and `Subscriber =
// handler::Annotation`.
impl Resolver for NoopMarshalResolver {
    type Key = handler::Key<Digest>;
    type Subscriber = handler::Annotation;

    fn fetch<F>(&mut self, _key: F) -> Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        Feedback::Ok
    }

    fn fetch_all<F>(&mut self, _keys: Vec<F>) -> Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        Feedback::Ok
    }

    fn retain(
        &mut self,
        _predicate: impl Fn(&Self::Key, &Self::Subscriber) -> bool + Send + 'static,
    ) -> Feedback {
        Feedback::Ok
    }
}

impl TargetedResolver for NoopMarshalResolver {
    type PublicKey = bls12381::PublicKey;

    fn fetch_targeted(
        &mut self,
        _fetch: impl Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
        _targets: NonEmptyVec<Self::PublicKey>,
    ) -> Feedback {
        Feedback::Ok
    }

    fn fetch_all_targeted<F>(&mut self, _keys: Vec<(F, NonEmptyVec<Self::PublicKey>)>) -> Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        Feedback::Ok
    }
}
