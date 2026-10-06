use crate::ExecutionScope;

/// Read-only body authority for execution in a particular scope.
pub struct ExecutionReaders<'scope, 'parent, P> {
    pub scope: &'scope ExecutionScope,
    pub parent: &'parent P,
}
