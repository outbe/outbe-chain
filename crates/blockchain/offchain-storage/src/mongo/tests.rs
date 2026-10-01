use std::time::Duration;

use mongodb::{
    bson::{doc, from_document},
    error::{CommandError, Error as MongoError, ErrorKind, WriteConcernError, WriteFailure},
    options::ClientOptions,
};

use super::{
    cap_execution_timeouts, map_operation_error, transaction_topology_supported,
    MongoStorageConfig, EXECUTION_READ_TIMEOUT,
};
use crate::StorageErrorKind;

#[test]
fn topology_capability_rejects_standalone_and_accepts_supported_deployments() {
    assert!(!transaction_topology_supported(&doc! {
        "isWritablePrimary": true,
        "logicalSessionTimeoutMinutes": 30,
    }));
    assert!(transaction_topology_supported(&doc! {
        "setName": "rs0",
        "logicalSessionTimeoutMinutes": 30,
    }));
    assert!(transaction_topology_supported(&doc! {
        "msg": "isdbgrid",
        "logicalSessionTimeoutMinutes": 30,
    }));
    assert!(!transaction_topology_supported(&doc! {
        "setName": "rs0",
    }));
}

#[test]
fn configuration_debug_redacts_mongodb_credentials() {
    let config = MongoStorageConfig {
        uri: "mongodb://user:secret@localhost:27017".to_owned(),
        database: "projection".to_owned(),
    };

    let debug = format!("{config:?}");
    assert!(!debug.contains("user:secret"));
    assert!(debug.contains("uri: \"<redacted>\""));
    assert!(debug.contains("database: \"projection\""));
}

#[test]
fn unsatisfied_majority_concerns_are_unavailable() {
    let read_concern_error: CommandError = from_document(doc! {
        "code": 134,
        "codeName": "ReadConcernMajorityNotAvailableYet",
        "errmsg": "majority read concern is temporarily unavailable",
    })
    .unwrap();
    let read_concern_error: MongoError = ErrorKind::Command(read_concern_error).into();
    assert_eq!(
        map_operation_error(read_concern_error).kind(),
        StorageErrorKind::Unavailable
    );

    let write_concern_error: WriteConcernError = from_document(doc! {
        "code": 64,
        "codeName": "WriteConcernFailed",
        "errmsg": "majority acknowledgement timed out",
    })
    .unwrap();
    let write_concern_error: MongoError =
        ErrorKind::Write(WriteFailure::WriteConcernError(write_concern_error)).into();
    assert_eq!(
        map_operation_error(write_concern_error).kind(),
        StorageErrorKind::Unavailable
    );
}

#[test]
fn transient_command_failures_are_unavailable() {
    for (code, code_name) in [
        (50, "MaxTimeMSExpired"),
        (91, "ShutdownInProgress"),
        (10_107, "NotWritablePrimary"),
        (11_602, "InterruptedDueToReplStateChange"),
    ] {
        let command: CommandError = from_document(doc! {
            "code": code,
            "codeName": code_name,
            "errmsg": "temporary topology or operation failure",
        })
        .unwrap();
        let error: MongoError = ErrorKind::Command(command).into();
        assert_eq!(
            map_operation_error(error).kind(),
            StorageErrorKind::Unavailable,
            "MongoDB command code {code} must enter recovery",
        );
    }
}

#[test]
fn deterministic_command_failure_remains_backend_failure() {
    let command: CommandError = from_document(doc! {
        "code": 13,
        "codeName": "Unauthorized",
        "errmsg": "not authorized",
    })
    .unwrap();
    let error: MongoError = ErrorKind::Command(command).into();
    assert_eq!(map_operation_error(error).kind(), StorageErrorKind::Backend,);
}

#[test]
fn execution_connection_attempts_cannot_exceed_the_one_second_read_budget() {
    let mut options = ClientOptions::default();
    options.server_selection_timeout = Some(Duration::from_secs(30));
    options.connect_timeout = Some(Duration::from_secs(15));

    cap_execution_timeouts(&mut options);

    assert_eq!(
        options.server_selection_timeout,
        Some(EXECUTION_READ_TIMEOUT)
    );
    assert_eq!(options.connect_timeout, Some(EXECUTION_READ_TIMEOUT));
}
