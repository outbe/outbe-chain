/// Whether to update head or finalized.
#[derive(Debug)]
pub(super) enum HeadOrFinalized {
    Head,
    Finalized,
}
