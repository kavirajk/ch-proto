#![cfg(feature = "integration")]

use ch_proto::connection::Connection;
use std::io;

const ADDR: &str = "127.0.0.1:9000";

/// Ensure ClickHouse is reachable before running tests that depend on it.
fn require_server() {
    Connection::connect(ADDR, None, None, None).expect(
        "ClickHouse server not running at 127.0.0.1:9000 — start it with `make up`",
    );
}

#[test]
fn test_connect_default() {
    require_server();
    let conn = Connection::connect(ADDR, None, None, None).unwrap();
    println!("conn: {conn:?}");
}

#[test]
fn test_connect_with_database() {
    require_server();
    let conn = Connection::connect(ADDR, Some("default"), None, None).unwrap();
    println!("conn: {conn:?}");
}

#[test]
fn test_connect_with_credentials() {
    require_server();
    let conn = Connection::connect(ADDR, None, Some("default"), Some("")).unwrap();
    println!("conn: {conn:?}");
}

#[test]
fn test_connect_wrong_password() {
    require_server();
    let result = Connection::connect(ADDR, None, Some("default"), Some("wrong_password"));
    let err = result.unwrap_err();
    // Should be a server-side exception, not a connection error
    assert_ne!(err.kind(), io::ErrorKind::ConnectionRefused);
}

#[test]
fn test_connect_nonexistent_user() {
    require_server();
    let result = Connection::connect(ADDR, None, Some("nonexistent_user_xyz"), None);
    let err = result.unwrap_err();
    assert_ne!(err.kind(), io::ErrorKind::ConnectionRefused);
}

#[test]
fn test_connect_wrong_address() {
    let result = Connection::connect("127.0.0.1:19999", None, None, None);
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
}

#[test]
fn test_multiple_connections() {
    require_server();
    let conn1 = Connection::connect(ADDR, None, None, None).unwrap();
    let conn2 = Connection::connect(ADDR, None, None, None).unwrap();
    println!("conn1: {conn1:?}");
    println!("conn2: {conn2:?}");
}
