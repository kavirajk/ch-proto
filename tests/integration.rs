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

// -- Tuple(...) --

#[test]
fn test_tuple_uint32_string_single_row() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Single-row Tuple(UInt32, String): (42, 'hi')
    let result = conn
        .query("SELECT (42, 'hi')::Tuple(UInt32, String)")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Tuple(elems) => {
            assert_eq!(elems.len(), 2);
            match &elems[0] {
                ch_proto::proto::column::ColumnData::Uint32(v) => {
                    assert_eq!(v, &vec![42u32]);
                }
                other => panic!("expected Uint32 element 0, got {other:?}"),
            }
            match &elems[1] {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v, &vec!["hi".to_string()]);
                }
                other => panic!("expected String element 1, got {other:?}"),
            }
        }
        other => panic!("expected Tuple, got {other:?}"),
    }
}

#[test]
fn test_tuple_multi_row() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Three rows of Tuple(UInt32, String): (10,'a'), (20,'bb'), (30,'ccc')
    let result = conn
        .query(
            "SELECT arrayJoin([\
                (10, 'a')::Tuple(UInt32, String), \
                (20, 'bb')::Tuple(UInt32, String), \
                (30, 'ccc')::Tuple(UInt32, String)\
             ])",
        )
        .unwrap();
    assert_eq!(result.row_count(), 3);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Tuple(elems) => {
            assert_eq!(elems.len(), 2);
            match &elems[0] {
                ch_proto::proto::column::ColumnData::Uint32(v) => {
                    assert_eq!(v, &vec![10u32, 20, 30]);
                }
                other => panic!("expected Uint32 element 0, got {other:?}"),
            }
            match &elems[1] {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v, &vec!["a".to_string(), "bb".to_string(), "ccc".to_string()]);
                }
                other => panic!("expected String element 1, got {other:?}"),
            }
        }
        other => panic!("expected Tuple, got {other:?}"),
    }
}

#[test]
fn test_tuple_nested() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Tuple(UInt8, Tuple(Int32, String)) single row: (1, (100, 'x'))
    let result = conn
        .query("SELECT (1, (100, 'x'))::Tuple(UInt8, Tuple(Int32, String))")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Tuple(elems) => {
            assert_eq!(elems.len(), 2);
            match &elems[0] {
                ch_proto::proto::column::ColumnData::Uint8(v) => assert_eq!(v, &vec![1u8]),
                other => panic!("expected Uint8 element 0, got {other:?}"),
            }
            match &elems[1] {
                ch_proto::proto::column::ColumnData::Tuple(inner) => {
                    assert_eq!(inner.len(), 2);
                    match &inner[0] {
                        ch_proto::proto::column::ColumnData::Int32(v) => {
                            assert_eq!(v, &vec![100i32]);
                        }
                        other => panic!("expected Int32, got {other:?}"),
                    }
                    match &inner[1] {
                        ch_proto::proto::column::ColumnData::String(v) => {
                            assert_eq!(v, &vec!["x".to_string()]);
                        }
                        other => panic!("expected String, got {other:?}"),
                    }
                }
                other => panic!("expected nested Tuple, got {other:?}"),
            }
        }
        other => panic!("expected outer Tuple, got {other:?}"),
    }
}

#[test]
fn test_tuple_with_array_inner() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Tuple(Array(UInt32), String) single row: ([1,2,3], 'hello')
    let result = conn
        .query("SELECT ([1, 2, 3], 'hello')::Tuple(Array(UInt32), String)")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Tuple(elems) => {
            assert_eq!(elems.len(), 2);
            match &elems[0] {
                ch_proto::proto::column::ColumnData::Array { inner, offsets } => {
                    assert_eq!(offsets, &vec![3u64]);
                    match inner.as_ref() {
                        ch_proto::proto::column::ColumnData::Uint32(v) => {
                            assert_eq!(v, &vec![1u32, 2, 3]);
                        }
                        other => panic!("expected Uint32 innermost, got {other:?}"),
                    }
                }
                other => panic!("expected Array element 0, got {other:?}"),
            }
            match &elems[1] {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v, &vec!["hello".to_string()]);
                }
                other => panic!("expected String element 1, got {other:?}"),
            }
        }
        other => panic!("expected Tuple, got {other:?}"),
    }
}

#[test]
fn test_tuple_with_nullable_inner() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Tuple(Nullable(UInt32), String) single row: (NULL, 'present')
    let result = conn
        .query("SELECT (CAST(NULL AS Nullable(UInt32)), 'present')::Tuple(Nullable(UInt32), String)")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Tuple(elems) => {
            assert_eq!(elems.len(), 2);
            match &elems[0] {
                ch_proto::proto::column::ColumnData::Nullable { nulls, .. } => {
                    assert_eq!(nulls, &vec![1u8]);
                }
                other => panic!("expected Nullable element 0, got {other:?}"),
            }
            match &elems[1] {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v, &vec!["present".to_string()]);
                }
                other => panic!("expected String element 1, got {other:?}"),
            }
        }
        other => panic!("expected Tuple, got {other:?}"),
    }
}

// -- Map(K, V) --

#[test]
fn test_map_string_uint32_single_row() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Map(String, UInt32) single row: {'a':1, 'b':2}
    let result = conn
        .query("SELECT map('a', 1, 'b', 2)::Map(String, UInt32)")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Map {
            keys,
            values,
            offsets,
        } => {
            assert_eq!(offsets, &vec![2u64]);
            match keys.as_ref() {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v, &vec!["a".to_string(), "b".to_string()]);
                }
                other => panic!("expected String keys, got {other:?}"),
            }
            match values.as_ref() {
                ch_proto::proto::column::ColumnData::Uint32(v) => {
                    assert_eq!(v, &vec![1u32, 2]);
                }
                other => panic!("expected Uint32 values, got {other:?}"),
            }
        }
        other => panic!("expected Map, got {other:?}"),
    }
}

#[test]
fn test_map_empty() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT map()::Map(String, UInt32)")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Map {
            keys,
            values,
            offsets,
        } => {
            assert_eq!(offsets, &vec![0u64]);
            assert_eq!(keys.row_count(), 0);
            assert_eq!(values.row_count(), 0);
        }
        other => panic!("expected Map, got {other:?}"),
    }
}

#[test]
fn test_map_multi_row() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Three rows: {'a':1,'b':2}, {}, {'c':3}
    let result = conn
        .query(
            "SELECT arrayJoin([\
                map('a', 1, 'b', 2)::Map(String, UInt32), \
                map()::Map(String, UInt32), \
                map('c', 3)::Map(String, UInt32)\
             ])",
        )
        .unwrap();
    assert_eq!(result.row_count(), 3);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Map {
            keys,
            values,
            offsets,
        } => {
            assert_eq!(offsets, &vec![2u64, 2, 3]);
            match keys.as_ref() {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v, &vec!["a".to_string(), "b".to_string(), "c".to_string()]);
                }
                other => panic!("expected String keys, got {other:?}"),
            }
            match values.as_ref() {
                ch_proto::proto::column::ColumnData::Uint32(v) => {
                    assert_eq!(v, &vec![1u32, 2, 3]);
                }
                other => panic!("expected Uint32 values, got {other:?}"),
            }
        }
        other => panic!("expected Map, got {other:?}"),
    }
}

#[test]
fn test_map_array_key() {
    // Regression: confirms KeyType can be a composite (Array). Previously
    // the SPEC incorrectly claimed scalar-only key types.
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Map(Array(String), Int8) single row: {['Kavi']: 9}
    let result = conn
        .query("SELECT map(['Kavi'], 9)::Map(Array(String), Int8)")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Map {
            keys,
            values,
            offsets,
        } => {
            assert_eq!(offsets, &vec![1u64]);
            match keys.as_ref() {
                ch_proto::proto::column::ColumnData::Array {
                    inner,
                    offsets: ko,
                } => {
                    assert_eq!(ko, &vec![1u64]);
                    match inner.as_ref() {
                        ch_proto::proto::column::ColumnData::String(v) => {
                            assert_eq!(v, &vec!["Kavi".to_string()]);
                        }
                        other => panic!("expected String innermost, got {other:?}"),
                    }
                }
                other => panic!("expected Array keys, got {other:?}"),
            }
            match values.as_ref() {
                ch_proto::proto::column::ColumnData::Int8(v) => {
                    assert_eq!(v, &vec![9i8]);
                }
                other => panic!("expected Int8 values, got {other:?}"),
            }
        }
        other => panic!("expected Map, got {other:?}"),
    }
}

// -- Nested(...) --
//
// Note: with default `flatten_nested = 1` (which is the server default),
// `Nested(...)` columns are physically stored and sent to the client as N
// flat `Array(T_i)` columns with dotted names — no `Nested` wire type is
// involved, and our existing `Array(T)` decoder already covers that path.
//
// These tests exercise the `flatten_nested = 0` shape, where the server
// genuinely emits a column with type string `Nested(name1 T1, ...)`. We
// reach that shape via `::Nested(...)` cast on a literal so the test is
// self-contained (no DDL required).

#[test]
fn test_nested_single_row_cast() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Single-row Nested(a UInt8, b String): row has 2 elements (10,'x'),(20,'y')
    let result = conn
        .query("SELECT [(10, 'x'), (20, 'y')]::Nested(a UInt8, b String) AS n")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Nested { fields, offsets } => {
            assert_eq!(offsets, &vec![2u64]);
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].0, "a");
            match &fields[0].1 {
                ch_proto::proto::column::ColumnData::Uint8(v) => {
                    assert_eq!(v, &vec![10u8, 20]);
                }
                other => panic!("expected Uint8 field a, got {other:?}"),
            }
            assert_eq!(fields[1].0, "b");
            match &fields[1].1 {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v, &vec!["x".to_string(), "y".to_string()]);
                }
                other => panic!("expected String field b, got {other:?}"),
            }
        }
        other => panic!("expected Nested, got {other:?}"),
    }
}

#[test]
fn test_nested_multi_row() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // 3 rows: row 0 has 2 elements, row 1 has 0, row 2 has 1.
    let result = conn
        .query(
            "SELECT arrayJoin([\
                [(10, 'x'), (20, 'y')]::Nested(a UInt8, b String), \
                []::Nested(a UInt8, b String), \
                [(30, 'z')]::Nested(a UInt8, b String)\
             ])",
        )
        .unwrap();
    assert_eq!(result.row_count(), 3);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Nested { fields, offsets } => {
            assert_eq!(offsets, &vec![2u64, 2, 3]);
            assert_eq!(fields.len(), 2);
            match &fields[0].1 {
                ch_proto::proto::column::ColumnData::Uint8(v) => {
                    assert_eq!(v, &vec![10u8, 20, 30]);
                }
                other => panic!("expected Uint8 field a, got {other:?}"),
            }
            match &fields[1].1 {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v, &vec!["x".to_string(), "y".to_string(), "z".to_string()]);
                }
                other => panic!("expected String field b, got {other:?}"),
            }
        }
        other => panic!("expected Nested, got {other:?}"),
    }
}

#[test]
fn test_nested_three_fields() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query(
            "SELECT [(1, 100, 'one'), (2, 200, 'two')]::Nested(x UInt8, y Int32, z String) AS n",
        )
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Nested { fields, offsets } => {
            assert_eq!(offsets, &vec![2u64]);
            let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(names, vec!["x", "y", "z"]);
            match &fields[0].1 {
                ch_proto::proto::column::ColumnData::Uint8(v) => assert_eq!(v, &vec![1u8, 2]),
                other => panic!("expected Uint8 field x, got {other:?}"),
            }
            match &fields[1].1 {
                ch_proto::proto::column::ColumnData::Int32(v) => assert_eq!(v, &vec![100i32, 200]),
                other => panic!("expected Int32 field y, got {other:?}"),
            }
            match &fields[2].1 {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v, &vec!["one".to_string(), "two".to_string()]);
                }
                other => panic!("expected String field z, got {other:?}"),
            }
        }
        other => panic!("expected Nested, got {other:?}"),
    }
}

#[test]
fn test_nested_with_array_field() {
    // Field type is itself a composite (Array). Verifies recursion through
    // the parser and decoder for Nested fields.
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query(
            "SELECT [(1, [10, 20]), (2, [30])]::Nested(a UInt8, b Array(UInt32)) AS n",
        )
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Nested { fields, offsets } => {
            assert_eq!(offsets, &vec![2u64]);
            assert_eq!(fields.len(), 2);
            match &fields[0].1 {
                ch_proto::proto::column::ColumnData::Uint8(v) => assert_eq!(v, &vec![1u8, 2]),
                other => panic!("expected Uint8 field a, got {other:?}"),
            }
            match &fields[1].1 {
                ch_proto::proto::column::ColumnData::Array { inner, offsets: bo } => {
                    assert_eq!(bo, &vec![2u64, 3]);
                    match inner.as_ref() {
                        ch_proto::proto::column::ColumnData::Uint32(v) => {
                            assert_eq!(v, &vec![10u32, 20, 30]);
                        }
                        other => panic!("expected Uint32 innermost, got {other:?}"),
                    }
                }
                other => panic!("expected Array field b, got {other:?}"),
            }
        }
        other => panic!("expected Nested, got {other:?}"),
    }
}

#[test]
fn test_map_complex_value_array() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // Map(String, Array(UInt32)) single row: {'a':[1,2], 'b':[]}
    let result = conn
        .query("SELECT map('a', [1, 2], 'b', []::Array(UInt32))::Map(String, Array(UInt32))")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Map {
            keys,
            values,
            offsets,
        } => {
            assert_eq!(offsets, &vec![2u64]);
            match keys.as_ref() {
                ch_proto::proto::column::ColumnData::String(v) => {
                    assert_eq!(v, &vec!["a".to_string(), "b".to_string()]);
                }
                other => panic!("expected String keys, got {other:?}"),
            }
            match values.as_ref() {
                ch_proto::proto::column::ColumnData::Array { inner, offsets: vo } => {
                    assert_eq!(vo, &vec![2u64, 2]);
                    match inner.as_ref() {
                        ch_proto::proto::column::ColumnData::Uint32(v) => {
                            assert_eq!(v, &vec![1u32, 2]);
                        }
                        other => panic!("expected Uint32 innermost, got {other:?}"),
                    }
                }
                other => panic!("expected Array values, got {other:?}"),
            }
        }
        other => panic!("expected Map, got {other:?}"),
    }
}

// =====================================================================
// Phase 7: fixed-width and parameterized types
// =====================================================================

// -- Int16 / Float32 / Float64 / Bool --

#[test]
fn test_int16_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT toInt16(-32768)").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Int16(v) => assert_eq!(v, &vec![-32768i16]),
        other => panic!("expected Int16, got {other:?}"),
    }
}

#[test]
fn test_float32_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT toFloat32(1.5)").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Float32(v) => assert_eq!(v, &vec![1.5f32]),
        other => panic!("expected Float32, got {other:?}"),
    }
}

#[test]
fn test_float64_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT toFloat64(1.5)").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Float64(v) => assert_eq!(v, &vec![1.5f64]),
        other => panic!("expected Float64, got {other:?}"),
    }
}

#[test]
fn test_bool_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT arrayJoin([true, false, true])").unwrap();
    assert_eq!(result.row_count(), 3);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Bool(v) => {
            assert_eq!(v, &vec![true, false, true]);
        }
        other => panic!("expected Bool, got {other:?}"),
    }
}

// -- Date / Date32 --

#[test]
fn test_date_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // 2024-01-15 = 19737 days since 1970-01-01.
    let result = conn.query("SELECT toDate('2024-01-15')").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Date(v) => assert_eq!(v, &vec![19737u16]),
        other => panic!("expected Date, got {other:?}"),
    }
}

#[test]
fn test_date32_select_pre_epoch() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // 1900-01-01 = -25567 days since 1970-01-01.
    let result = conn.query("SELECT toDate32('1900-01-01')").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Date32(v) => assert_eq!(v, &vec![-25567i32]),
        other => panic!("expected Date32, got {other:?}"),
    }
}

// -- DateTime64 --

#[test]
fn test_datetime64_scale_3_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT toDateTime64('2024-01-15 12:30:45.123', 3, 'UTC')")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::DateTime64 { scale, values } => {
            assert_eq!(*scale, 3);
            assert_eq!(values, &vec![1705321845123i64]);
        }
        other => panic!("expected DateTime64, got {other:?}"),
    }
}

#[test]
fn test_datetime64_scale_0_no_tz() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT toDateTime64('2024-01-15 12:30:45', 0)")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::DateTime64 { scale, values } => {
            assert_eq!(*scale, 0);
            assert_eq!(values, &vec![1705321845i64]);
        }
        other => panic!("expected DateTime64, got {other:?}"),
    }
}

// -- UUID --

#[test]
fn test_uuid_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT toUUID('550e8400-e29b-41d4-a716-446655440000')")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Uuid(v) => {
            let expected =
                uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
            assert_eq!(v, &vec![expected]);
        }
        other => panic!("expected UUID, got {other:?}"),
    }
}

#[test]
fn test_uuid_zero() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT toUUID('00000000-0000-0000-0000-000000000000')")
        .unwrap();
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Uuid(v) => {
            assert_eq!(v, &vec![uuid::Uuid::nil()]);
        }
        other => panic!("expected UUID, got {other:?}"),
    }
}

// -- IPv4 / IPv6 --

#[test]
fn test_ipv4_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT toIPv4('192.168.1.10')").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Ipv4(v) => {
            // 192.168.1.10 as u32 in network/canonical order.
            assert_eq!(v, &vec![0xC0A8010Au32]);
        }
        other => panic!("expected IPv4, got {other:?}"),
    }
}

#[test]
fn test_ipv6_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT toIPv6('2001:db8::1')").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Ipv6(v) => {
            assert_eq!(v.len(), 1);
            // 2001:db8::1 in network byte order.
            let mut expected = [0u8; 16];
            expected[0] = 0x20;
            expected[1] = 0x01;
            expected[2] = 0x0D;
            expected[3] = 0xB8;
            expected[15] = 0x01;
            assert_eq!(v[0], expected);
        }
        other => panic!("expected IPv6, got {other:?}"),
    }
}

// -- Enum16 --

#[test]
fn test_enum16_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT CAST(30000 AS Enum16('a' = 1, 'b' = 30000))")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Enum16(v) => assert_eq!(v, &vec![30000i16]),
        other => panic!("expected Enum16, got {other:?}"),
    }
}

// -- Decimal --

#[test]
fn test_decimal32_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    // 123.4567 with scale 4 → underlying 1234567.
    let result = conn.query("SELECT toDecimal32('123.4567', 4)").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Decimal32 { scale, values } => {
            assert_eq!(*scale, 4);
            assert_eq!(values, &vec![1234567i32]);
        }
        other => panic!("expected Decimal32, got {other:?}"),
    }
}

#[test]
fn test_decimal64_select_negative() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT toDecimal64('-1.5', 1)").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Decimal64 { scale, values } => {
            assert_eq!(*scale, 1);
            assert_eq!(values, &vec![-15i64]);
        }
        other => panic!("expected Decimal64, got {other:?}"),
    }
}

#[test]
fn test_decimal128_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT toDecimal128('123.4567', 4)").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Decimal128 { scale, values } => {
            assert_eq!(*scale, 4);
            assert_eq!(values, &vec![1234567i128]);
        }
        other => panic!("expected Decimal128, got {other:?}"),
    }
}

#[test]
fn test_decimal256_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT toDecimal256('123.4567', 4)").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Decimal256 { scale, values } => {
            assert_eq!(*scale, 4);
            assert_eq!(values.len(), 1);
            // 1234567 little-endian in the first 4 bytes, rest zero.
            let mut expected = [0u8; 32];
            expected[0] = 0x87;
            expected[1] = 0xD6;
            expected[2] = 0x12;
            assert_eq!(values[0], expected);
        }
        other => panic!("expected Decimal256, got {other:?}"),
    }
}

// -- Int128 / UInt128 / Int256 / UInt256 --

#[test]
fn test_int128_max() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT toInt128('170141183460469231731687303715884105727')")
        .unwrap();
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Int128(v) => {
            assert_eq!(v, &vec![i128::MAX]);
        }
        other => panic!("expected Int128, got {other:?}"),
    }
}

#[test]
fn test_uint128_max() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT toUInt128('340282366920938463463374607431768211455')")
        .unwrap();
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Uint128(v) => assert_eq!(v, &vec![u128::MAX]),
        other => panic!("expected UInt128, got {other:?}"),
    }
}

#[test]
fn test_int256_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT toInt256('123')").unwrap();
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Int256(v) => {
            let mut expected = [0u8; 32];
            expected[0] = 0x7B;
            assert_eq!(v, &vec![expected]);
        }
        other => panic!("expected Int256, got {other:?}"),
    }
}

#[test]
fn test_uint256_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT toUInt256('123')").unwrap();
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Uint256(v) => {
            let mut expected = [0u8; 32];
            expected[0] = 0x7B;
            assert_eq!(v, &vec![expected]);
        }
        other => panic!("expected UInt256, got {other:?}"),
    }
}

// -- Composability with Phase 7 types --

#[test]
fn test_array_of_uuid() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query(
            "SELECT [\
                toUUID('550e8400-e29b-41d4-a716-446655440000'), \
                toUUID('00000000-0000-0000-0000-000000000000')\
             ]",
        )
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Array { inner, offsets } => {
            assert_eq!(offsets, &vec![2u64]);
            match inner.as_ref() {
                ch_proto::proto::column::ColumnData::Uuid(v) => {
                    let u =
                        uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
                    assert_eq!(v, &vec![u, uuid::Uuid::nil()]);
                }
                other => panic!("expected UUID inner, got {other:?}"),
            }
        }
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn test_nullable_decimal32() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query(
            "SELECT arrayJoin([\
                CAST(toDecimal32('1.23', 2) AS Nullable(Decimal(9, 2))), \
                CAST(NULL AS Nullable(Decimal(9, 2)))\
             ])",
        )
        .unwrap();
    assert_eq!(result.row_count(), 2);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Nullable { inner, nulls } => {
            assert_eq!(nulls, &vec![0u8, 1]);
            match inner.as_ref() {
                ch_proto::proto::column::ColumnData::Decimal32 { scale, values } => {
                    assert_eq!(*scale, 2);
                    assert_eq!(values[0], 123);
                }
                other => panic!("expected Decimal32 inner, got {other:?}"),
            }
        }
        other => panic!("expected Nullable, got {other:?}"),
    }
}
