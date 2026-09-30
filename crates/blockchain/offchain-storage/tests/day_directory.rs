use outbe_offchain_storage::{
    DayDirectory, Key, Namespace, RocksDbConfig, RocksDbStorage, StorageBackend, StorageConfig,
    StorageErrorKind, StorageProvider, StorageReader, StorageWriter, Value,
};

fn records() -> (Namespace, Key, Value) {
    (
        Namespace::new("records").unwrap(),
        Key::new(b"kept".to_vec()).unwrap(),
        Value::new(b"from-day".to_vec()).unwrap(),
    )
}

#[test]
fn day_seven_does_not_open_day_eight() {
    let root = tempfile::tempdir().unwrap();
    let days = DayDirectory::open(root.path()).unwrap();
    let (namespace, key, value) = records();
    {
        let day = days.open_tribute_day(7).unwrap();
        day.put(namespace.clone(), &key, &value).unwrap();
    }
    assert!(!root.path().join("tribute-days/8").exists());
    assert!(!root.path().join("nod-days/8").exists());
    {
        let day = days.open_tribute_day(8).unwrap();
        assert!(day.get(namespace.clone(), &key).unwrap().is_none());
    }
    let day = days.open_tribute_day(7).unwrap();
    assert_eq!(day.get(namespace, &key).unwrap(), Some(value));
}

#[test]
fn drop_missing_directory_is_ok() {
    let root = tempfile::tempdir().unwrap();
    let days = DayDirectory::open(root.path()).unwrap();
    days.drop_tribute_day(7).unwrap();
    days.drop_nod_day(7).unwrap();
    {
        let day = days.open_tribute_day(7).unwrap();
        let (namespace, key, value) = records();
        day.put(namespace, &key, &value).unwrap();
    }
    days.drop_tribute_day(7).unwrap();
    assert!(!root.path().join("tribute-days/7").exists());
    days.drop_tribute_day(7).unwrap();
    days.drop_nod_day(7).unwrap();
}

#[test]
fn legacy_current_moves_into_shared() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("projection");
    let (namespace, key, value) = records();
    {
        let storage = RocksDbStorage::open(&path).unwrap();
        storage.put(namespace.clone(), &key, &value).unwrap();
    }
    assert!(path.join("CURRENT").is_file());

    let days = DayDirectory::open(&path).unwrap();
    assert!(!path.join("CURRENT").is_file());
    assert!(days.shared_path().join("CURRENT").is_file());
    {
        let shared = days.open_shared().unwrap();
        assert_eq!(
            shared.get(namespace.clone(), &key).unwrap(),
            Some(value.clone())
        );
    }

    let opened = StorageProvider::new(StorageConfig {
        start_block: 1,
        backend: StorageBackend::RocksDb(RocksDbConfig {
            path: path.clone(),
            secondary_path: root.path().join("secondary"),
        }),
    })
    .unwrap()
    .open_writer()
    .unwrap();
    assert_eq!(opened.reader.get(namespace, &key).unwrap(), Some(value));
    assert!(!path.join("CURRENT").is_file());
}

#[test]
fn current_and_shared_together_refuse_to_open() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("projection");
    {
        let storage = RocksDbStorage::open(&path).unwrap();
        let (namespace, key, value) = records();
        storage.put(namespace, &key, &value).unwrap();
    }
    std::fs::create_dir(path.join("shared")).unwrap();
    std::fs::write(path.join("shared/CURRENT"), b"MANIFEST-000001\n").unwrap();

    let error = DayDirectory::open(&path).unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Corruption);
    let provider = StorageProvider::new(StorageConfig {
        start_block: 1,
        backend: StorageBackend::RocksDb(RocksDbConfig {
            path: path.clone(),
            secondary_path: root.path().join("secondary"),
        }),
    })
    .unwrap()
    .open_writer();
    let Err(provider_error) = provider else {
        panic!("provider opened a root that has both a legacy database and shared/");
    };
    assert_eq!(provider_error.kind(), StorageErrorKind::Corruption);
    assert!(path.join("CURRENT").is_file());
}
