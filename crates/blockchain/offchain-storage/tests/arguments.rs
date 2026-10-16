use outbe_offchain_storage::{
    Key, Namespace, ScanRequest, StorageErrorKind, Value, MAX_KEY_BYTES, MAX_NAMESPACE_BYTES,
    MAX_SCAN_ENTRIES, MAX_VALUE_BYTES,
};

#[test]
fn wrapper_boundaries_are_enforced_before_backend_access() {
    let max_namespace = format!("a{}", "b".repeat(MAX_NAMESPACE_BYTES - 1));
    assert!(Namespace::new(max_namespace).is_ok());
    assert_invalid(Namespace::new(""));
    assert_invalid(Namespace::new("Uppercase"));
    assert_invalid(Namespace::new("unsafe-name"));
    assert_invalid(Namespace::new(format!(
        "a{}",
        "b".repeat(MAX_NAMESPACE_BYTES)
    )));

    assert!(Key::new(vec![0; MAX_KEY_BYTES]).is_ok());
    assert_invalid(Key::new(Vec::new()));
    assert_invalid(Key::new(vec![0; MAX_KEY_BYTES + 1]));

    assert!(Value::new(vec![0; MAX_VALUE_BYTES]).is_ok());
    assert_invalid(Value::new(vec![0; MAX_VALUE_BYTES + 1]));
}

#[test]
fn scan_request_boundaries_and_cursor_prefix_are_enforced() {
    let cursor = Key::new(b"ab-cursor".to_vec()).unwrap();
    assert!(ScanRequest::new(&vec![0; MAX_KEY_BYTES], None, MAX_SCAN_ENTRIES).is_ok());
    assert!(ScanRequest::new(b"ab", Some(&cursor), 1).is_ok());

    assert_invalid(ScanRequest::new(&vec![0; MAX_KEY_BYTES + 1], None, 1));
    assert_invalid(ScanRequest::new(&[], None, 0));
    assert_invalid(ScanRequest::new(&[], None, MAX_SCAN_ENTRIES + 1));
    assert_invalid(ScanRequest::new(b"different", Some(&cursor), 1));
}

fn assert_invalid<T: std::fmt::Debug>(result: Result<T, outbe_offchain_storage::StorageError>) {
    assert_eq!(
        result.expect_err("argument must be rejected").kind(),
        StorageErrorKind::InvalidArgument
    );
}
