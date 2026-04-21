#![cfg(feature = "integration")]

use ch_proto::client::Connection;
use ch_proto::options::QueryOptions;
use ch_proto::proto::query::Stage;
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

// -- QueryOptions tests --

#[test]
fn test_query_with_custom_id() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let opts = QueryOptions::new().with_query_id("my-custom-query-id-123");
    let result = conn.query_with("SELECT 1", opts).unwrap();
    assert_eq!(result.row_count(), 1);
}

#[test]
fn test_query_with_stage_complete() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let opts = QueryOptions::new().with_stage(Stage::Complete);
    let result = conn.query_with("SELECT 1", opts).unwrap();
    assert_eq!(result.row_count(), 1);
}

#[test]
fn test_query_with_stage_fetch_columns() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let opts = QueryOptions::new().with_stage(Stage::FetchColumns);
    // Just ensure the query completes without error. The effect of
    // FetchColumns depends on query planning and is not strictly observable
    // for trivial queries.
    let _ = conn.query_with("SELECT 1", opts).unwrap();
}

#[test]
fn test_query_with_single_setting() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let opts = QueryOptions::new().with_setting("max_threads", "1");
    let result = conn.query_with("SELECT 1", opts).unwrap();
    assert_eq!(result.row_count(), 1);
}

#[test]
fn test_query_with_multiple_settings() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let opts = QueryOptions::new()
        .with_setting("max_threads", "2")
        .with_setting("max_memory_usage", "1000000000");
    let result = conn.query_with("SELECT 1", opts).unwrap();
    assert_eq!(result.row_count(), 1);
}

#[test]
fn test_query_with_param() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let opts = QueryOptions::new().with_param("x", "42");
    let result = conn.query_with("SELECT {x:UInt32}", opts).unwrap();

    assert_eq!(result.row_count(), 1);
    // Verify the value came back as 42.
    let first_block = &result.rows[0];
    match &first_block.columns[0].data {
        ch_proto::proto::column::ColumnData::Uint32(v) => assert_eq!(v[0], 42),
        other => panic!("expected Uint32 column, got {other:?}"),
    }
}

#[test]
fn test_query_with_multiple_params() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let opts = QueryOptions::new()
        .with_param("a", "10")
        .with_param("b", "20");
    // UInt32 + UInt32 promotes to UInt64 server-side.
    let result = conn
        .query_with("SELECT {a:UInt32} + {b:UInt32}", opts)
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Uint64(v) => assert_eq!(v[0], 30),
        other => panic!("expected Uint64 column, got {other:?}"),
    }
}

#[test]
fn test_query_with_missing_param() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // No parameter binding — server should reject.
    let opts = QueryOptions::new();
    let result = conn.query_with("SELECT {x:UInt32}", opts);
    assert!(result.is_err());
}

#[test]
fn test_query_default_options() {
    // query() delegates to query_with(QueryOptions::new()); should be identical behavior.
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let a = conn.query("SELECT 1").unwrap();
    let b = conn.query_with("SELECT 1", QueryOptions::new()).unwrap();
    assert_eq!(a.row_count(), b.row_count());
}

#[test]
fn test_query_string_type() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let a = conn.query("select name from system.tables").unwrap();

    eprintln!("databases: {:?}", a.rows)
}

#[test]
fn test_query_date_type() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let a = conn.query("select now()::Time64").unwrap();

    eprintln!("now: {:?}", a.rows)
}
