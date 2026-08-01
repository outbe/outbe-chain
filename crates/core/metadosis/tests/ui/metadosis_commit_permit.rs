use outbe_metadosis::commit::CommitPermit;

fn main() {
    let _ = std::any::type_name::<CommitPermit<'static>>();
}
