#![cfg(feature = "integration")]

use ch_proto::client::Connection;
use std::io;

const ADDR: &str = "127.0.0.1:9000";

/// Ensure ClickHouse is reachable before running tests that depend on it.
fn require_server() {
    Connection::connect(ADDR, None, None, None)
        .expect("ClickHouse server not running at 127.0.0.1:9000 — start it with `make up`");
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

#[test]
fn test_ping() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    conn.ping().unwrap();
}

#[test]
fn test_ping_multiple() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    for _ in 0..10 {
        conn.ping().unwrap();
    }
}

#[test]
fn test_ping_after_reconnect() {
    require_server();
    let mut conn1 = Connection::connect(ADDR, None, None, None).unwrap();
    conn1.ping().unwrap();
    drop(conn1);

    let mut conn2 = Connection::connect(ADDR, None, None, None).unwrap();
    conn2.ping().unwrap();
}

#[test]
fn test_ping_multiple_connections() {
    require_server();
    let mut conn1 = Connection::connect(ADDR, None, None, None).unwrap();
    let mut conn2 = Connection::connect(ADDR, None, None, None).unwrap();
    conn1.ping().unwrap();
    conn2.ping().unwrap();
    conn1.ping().unwrap();
    conn2.ping().unwrap();
}

#[test]
fn test_simple_query() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    conn.ping().unwrap();
    let result = conn.query("SELECT 1").unwrap();

    if let Some(header) = &result.header {
        println!("schema: {} columns", header.cols_count());
        for c in &header.columns {
            println!("  {} {}", c.name, c.data_type);
        }
    }

    println!("total rows: {}", result.row_count());
    for b in &result.rows {
        println!("block: rows={} cols={}", b.rows_count, b.cols_count());
        for c in &b.columns {
            println!("  {} ({}): {:?}", c.name, c.data_type, c.data);
        }
    }

    if let Some(pi) = &result.profile {
        println!("profile: {:?}", pi);
    }
}
