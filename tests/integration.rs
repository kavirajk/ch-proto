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

// -- String type --

#[test]
fn test_string_basic() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT 'hello'").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::String(v) => assert_eq!(v[0], "hello"),
        other => panic!("expected String, got {other:?}"),
    }
}

#[test]
fn test_string_empty() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT ''").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::String(v) => assert_eq!(v[0], ""),
        other => panic!("expected String, got {other:?}"),
    }
}

#[test]
fn test_string_unicode() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT '日本語テスト'").unwrap();
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::String(v) => assert_eq!(v[0], "日本語テスト"),
        other => panic!("expected String, got {other:?}"),
    }
}

#[test]
fn test_string_large() {
    // 10000 chars — well past any VarUInt length-prefix boundary.
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT repeat('x', 10000)").unwrap();
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::String(v) => {
            assert_eq!(v[0].len(), 10000);
            assert!(v[0].chars().all(|c| c == 'x'));
        }
        other => panic!("expected String, got {other:?}"),
    }
}

#[test]
fn test_string_embedded_nul() {
    // ClickHouse Strings are byte sequences; NUL (0x00) is a valid byte.
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT 'ab\\0cd'").unwrap();
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::String(v) => {
            assert_eq!(v[0].as_bytes(), b"ab\x00cd");
        }
        other => panic!("expected String, got {other:?}"),
    }
}

// -- FixedString(N) type --

#[test]
fn test_fixed_string_basic() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // 'abc' cast to FixedString(5) — server right-pads with NUL to 5 bytes.
    let result = conn.query("SELECT CAST('abc' AS FixedString(5))").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::FixedString { n, data } => {
            assert_eq!(*n, 5);
            assert_eq!(data, &vec![b'a', b'b', b'c', 0x00, 0x00]);
        }
        other => panic!("expected FixedString, got {other:?}"),
    }
}

#[test]
fn test_fixed_string_exact_length() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT CAST('hello' AS FixedString(5))")
        .unwrap();
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::FixedString { n, data } => {
            assert_eq!(*n, 5);
            assert_eq!(data, b"hello");
        }
        other => panic!("expected FixedString, got {other:?}"),
    }
}

#[test]
fn test_fixed_string_various_sizes() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    for size in [1, 2, 16, 32, 100] {
        let sql = format!("SELECT CAST('x' AS FixedString({size}))");
        let result = conn.query(&sql).unwrap();
        match &result.rows[0].columns[0].data {
            ch_proto::proto::column::ColumnData::FixedString { n, data } => {
                assert_eq!(*n, size, "size mismatch for FixedString({size})");
                assert_eq!(
                    data.len(),
                    size,
                    "data length mismatch for FixedString({size})"
                );
                assert_eq!(data[0], b'x');
                // rest is NUL padding
                assert!(data[1..].iter().all(|&b| b == 0));
            }
            other => panic!("expected FixedString({size}), got {other:?}"),
        }
    }
}

#[test]
fn test_fixed_string_multiple_rows() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT CAST(x AS FixedString(4)) FROM (SELECT arrayJoin(['a', 'bb', 'ccc', 'dddd']) AS x)")
        .unwrap();
    assert_eq!(result.row_count(), 4);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::FixedString { n, data } => {
            assert_eq!(*n, 4);
            assert_eq!(data.len(), 16); // 4 rows × 4 bytes
            assert_eq!(&data[0..4], &[b'a', 0, 0, 0]);
            assert_eq!(&data[4..8], &[b'b', b'b', 0, 0]);
            assert_eq!(&data[8..12], &[b'c', b'c', b'c', 0]);
            assert_eq!(&data[12..16], b"dddd");
        }
        other => panic!("expected FixedString, got {other:?}"),
    }
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

// -- Nullable(T) --

#[test]
fn test_nullable_uint32_all_nulls() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT CAST(NULL AS Nullable(UInt32))")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Nullable { nulls, .. } => {
            assert_eq!(nulls, &vec![1u8]);
        }
        other => panic!("expected Nullable, got {other:?}"),
    }
}

#[test]
fn test_nullable_uint32_no_nulls() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT CAST(42 AS Nullable(UInt32))")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Nullable { inner, nulls } => {
            assert_eq!(nulls, &vec![0u8]);
            match inner.as_ref() {
                ch_proto::proto::column::ColumnData::Uint32(v) => assert_eq!(v[0], 42),
                other => panic!("expected Uint32 inner, got {other:?}"),
            }
        }
        other => panic!("expected Nullable, got {other:?}"),
    }
}

#[test]
fn test_nullable_uint32_mixed() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // 3 rows: [42, null, 100]
    let result = conn
        .query(
            "SELECT arrayJoin([CAST(42 AS Nullable(UInt32)), CAST(NULL AS Nullable(UInt32)), CAST(100 AS Nullable(UInt32))])",
        )
        .unwrap();
    assert_eq!(result.row_count(), 3);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Nullable { inner, nulls } => {
            assert_eq!(nulls, &vec![0u8, 1u8, 0u8]);
            match inner.as_ref() {
                ch_proto::proto::column::ColumnData::Uint32(v) => {
                    assert_eq!(v.len(), 3);
                    assert_eq!(v[0], 42);
                    // v[1] is a placeholder — server may emit any bytes
                    assert_eq!(v[2], 100);
                }
                other => panic!("expected Uint32 inner, got {other:?}"),
            }
        }
        other => panic!("expected Nullable, got {other:?}"),
    }
}

#[test]
fn test_nullable_string() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query(
            "SELECT arrayJoin([CAST('hi' AS Nullable(String)), CAST(NULL AS Nullable(String)), CAST('yo' AS Nullable(String))])",
        )
        .unwrap();
    assert_eq!(result.row_count(), 3);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Nullable { inner, nulls } => {
            assert_eq!(nulls, &vec![0u8, 1u8, 0u8]);
            match inner.as_ref() {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v.len(), 3);
                    assert_eq!(v[0], "hi");
                    // v[1] is a placeholder (empty string) for the null row
                    assert_eq!(v[2], "yo");
                }
                other => panic!("expected String inner, got {other:?}"),
            }
        }
        other => panic!("expected Nullable, got {other:?}"),
    }
}

// -- Array(T) --

#[test]
fn test_array_uint32_single_row() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT [10, 20, 30]::Array(UInt32)").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Array { inner, offsets } => {
            assert_eq!(offsets, &vec![3u64]);
            match inner.as_ref() {
                ch_proto::proto::column::ColumnData::Uint32(v) => assert_eq!(v, &vec![10u32, 20, 30]),
                other => panic!("expected Uint32 inner, got {other:?}"),
            }
        }
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn test_array_empty() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT []::Array(UInt32)").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Array { inner, offsets } => {
            assert_eq!(offsets, &vec![0u64]);
            match inner.as_ref() {
                ch_proto::proto::column::ColumnData::Uint32(v) => assert!(v.is_empty()),
                other => panic!("expected Uint32 inner, got {other:?}"),
            }
        }
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn test_array_multiple_rows() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Three rows: [[10,20,30], [], [40,50]]
    let result = conn
        .query("SELECT arrayJoin([[10, 20, 30]::Array(UInt32), []::Array(UInt32), [40, 50]::Array(UInt32)])")
        .unwrap();
    assert_eq!(result.row_count(), 3);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Array { inner, offsets } => {
            assert_eq!(offsets, &vec![3u64, 3, 5]);
            match inner.as_ref() {
                ch_proto::proto::column::ColumnData::Uint32(v) => {
                    assert_eq!(v, &vec![10u32, 20, 30, 40, 50]);
                }
                other => panic!("expected Uint32 inner, got {other:?}"),
            }
        }
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn test_array_string() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT ['a', 'bb', 'ccc']::Array(String)").unwrap();
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Array { inner, offsets } => {
            assert_eq!(offsets, &vec![3u64]);
            match inner.as_ref() {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v, &vec!["a".to_string(), "bb".to_string(), "ccc".to_string()]);
                }
                other => panic!("expected String inner, got {other:?}"),
            }
        }
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn test_array_nested() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Array(Array(UInt32)) with a single row containing [[1,2], [], [3]]
    let result = conn
        .query("SELECT [[1, 2], []::Array(UInt32), [3]]::Array(Array(UInt32))")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Array { inner, offsets } => {
            // Outer row count = 1, last offset = 3 (3 inner arrays in this single row).
            assert_eq!(offsets, &vec![3u64]);
            match inner.as_ref() {
                ch_proto::proto::column::ColumnData::Array {
                    inner: inner2,
                    offsets: mid_offsets,
                } => {
                    // Three inner arrays with element counts [2, 0, 1] → cumulative [2, 2, 3]
                    assert_eq!(mid_offsets, &vec![2u64, 2, 3]);
                    match inner2.as_ref() {
                        ch_proto::proto::column::ColumnData::Uint32(v) => {
                            assert_eq!(v, &vec![1u32, 2, 3]);
                        }
                        other => panic!("expected Uint32 innermost, got {other:?}"),
                    }
                }
                other => panic!("expected middle Array, got {other:?}"),
            }
        }
        other => panic!("expected outer Array, got {other:?}"),
    }
}

#[test]
fn test_array_of_nullable() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Array(Nullable(UInt32)) with [1, null, 3]
    let result = conn
        .query("SELECT [1, NULL, 3]::Array(Nullable(UInt32))")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Array { inner, offsets } => {
            assert_eq!(offsets, &vec![3u64]);
            match inner.as_ref() {
                ch_proto::proto::column::ColumnData::Nullable { nulls, .. } => {
                    assert_eq!(nulls, &vec![0u8, 1, 0]);
                }
                other => panic!("expected Nullable inner, got {other:?}"),
            }
        }
        other => panic!("expected Array, got {other:?}"),
    }
}
