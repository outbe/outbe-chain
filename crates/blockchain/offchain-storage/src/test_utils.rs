/// Implements the three read-through methods for a test storage adapter.
///
/// Routing introspection and convenience methods retain the trait defaults.
#[macro_export]
macro_rules! impl_test_storage_reader {
    ($adapter:ty, $field:ident) => {
        impl $crate::StorageReader for $adapter {
            fn get_record(
                &self,
                namespace: $crate::Namespace,
                key: &$crate::Key,
            ) -> ::core::result::Result<
                ::core::option::Option<$crate::StoredValue>,
                $crate::StorageError,
            > {
                $crate::StorageReader::get_record(&self.$field, namespace, key)
            }

            fn get_records(
                &self,
                namespace: $crate::Namespace,
                keys: &[$crate::Key],
            ) -> ::core::result::Result<
                ::std::vec::Vec<::core::option::Option<$crate::StoredValue>>,
                $crate::StorageError,
            > {
                $crate::StorageReader::get_records(&self.$field, namespace, keys)
            }

            fn scan_prefix(
                &self,
                namespace: $crate::Namespace,
                request: $crate::ScanRequest<'_>,
            ) -> ::core::result::Result<$crate::ScanPage, $crate::StorageError> {
                $crate::StorageReader::scan_prefix(&self.$field, namespace, request)
            }
        }
    };
}
