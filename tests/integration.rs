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
fn test_handshake_at_v54461_aligns_stream_for_query() {
    // At negotiated protocol >= 54461 the ServerHello carries a trailing
    // `password_complexity_rules` block (VarUInt count + N × (String, String)).
    // If the decoder consumed too few or too many bytes for this field, the
    // very next packet read — the response to any query — would misalign and
    // fail.  Use SELECT 1 as the cheapest probe.
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT 1").unwrap();
    assert!(
        result.header.is_some(),
        "expected a header block after SELECT 1 — missing header strongly suggests \
         the v54461 ServerHello decode misaligned the stream"
    );
    assert_eq!(result.row_count(), 1, "SELECT 1 must return exactly one row");
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

// =====================================================================
// Phase 8: Versioned/stateful types (subset)
// =====================================================================

// -- JSON (Tier 1, String fallback) --
//
// The client auto-injects `output_format_native_write_json_as_string=1`
// so JSON columns always come back as Tier 1 (a state-prefix Int64 = 1
// followed by N rows of String). See SPEC §8.4.2.1.

#[test]
fn test_json_tier1_simple() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn.query("SELECT '{\"a\":1}'::JSON").unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Json(v) => {
            // Server may re-stringify integers as JSON strings ("1") in this
            // mode — value is JSON text, so just check it's non-empty and
            // contains the key.
            assert_eq!(v.len(), 1);
            assert!(v[0].contains("\"a\""));
        }
        other => panic!("expected JSON, got {other:?}"),
    }
}

#[test]
fn test_json_tier1_multi_row() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT arrayJoin(['{\"x\":1}'::JSON, '{\"y\":2}'::JSON, '{}'::JSON])")
        .unwrap();
    assert_eq!(result.row_count(), 3);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::Json(v) => {
            assert_eq!(v.len(), 3);
            assert!(v[0].contains("\"x\""));
            assert!(v[1].contains("\"y\""));
            assert_eq!(v[2], "{}");
        }
        other => panic!("expected JSON, got {other:?}"),
    }
}

// -- LowCardinality(T) --

#[test]
fn test_lowcardinality_string_select() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT CAST('hello' AS LowCardinality(String))")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::LowCardinality { dict, keys, .. } => {
            // Dict has at least 2 entries (placeholder + "hello"); keys has one
            // entry that indexes to "hello".
            assert_eq!(keys.len(), 1);
            match dict.as_ref() {
                ch_proto::proto::column::ColumnData::String(v) => {
                    let key_idx = keys[0] as usize;
                    assert_eq!(v[key_idx], "hello");
                }
                other => panic!("expected String dict, got {other:?}"),
            }
        }
        other => panic!("expected LowCardinality, got {other:?}"),
    }
}

#[test]
fn test_lowcardinality_multi_row() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query(
            "SELECT arrayJoin(['a', 'b', 'a', 'c', 'b'])::LowCardinality(String)",
        )
        .unwrap();
    assert_eq!(result.row_count(), 5);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::LowCardinality { dict, keys, .. } => {
            assert_eq!(keys.len(), 5);
            let dict_strings = match dict.as_ref() {
                ch_proto::proto::column::ColumnData::String(v) => v.clone(),
                other => panic!("expected String dict, got {other:?}"),
            };
            // Reconstruct logical values via dict indirection.
            let logical: Vec<&str> = keys
                .iter()
                .map(|&k| dict_strings[k as usize].as_str())
                .collect();
            assert_eq!(logical, vec!["a", "b", "a", "c", "b"]);
        }
        other => panic!("expected LowCardinality, got {other:?}"),
    }
}

#[test]
fn test_lowcardinality_fixed_string() {
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT CAST('abc' AS LowCardinality(FixedString(3)))")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::LowCardinality { dict, keys, .. } => {
            assert_eq!(keys.len(), 1);
            match dict.as_ref() {
                ch_proto::proto::column::ColumnData::FixedString { n, data } => {
                    assert_eq!(*n, 3);
                    assert!(!data.is_empty());
                }
                other => panic!("expected FixedString dict, got {other:?}"),
            }
        }
        other => panic!("expected LowCardinality, got {other:?}"),
    }
}

#[test]
fn test_lowcardinality_nullable_inner() {
    // LC(Nullable(T)) encodes the dict as plain T on the wire — there's no
    // null-map stream. By convention dict[0] is an empty placeholder and
    // dict[1] is the null marker. Real values start at dict[2..].
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query("SELECT CAST('hi' AS LowCardinality(Nullable(String)))")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    match &result.rows[0].columns[0].data {
        ch_proto::proto::column::ColumnData::LowCardinality { dict, keys, .. } => {
            assert_eq!(keys.len(), 1);
            match dict.as_ref() {
                ch_proto::proto::column::ColumnData::String(v) => {
                    // Dict has 3 entries: [empty placeholder, null marker, "hi"].
                    // Key indexes into "hi" (at index 2).
                    assert_eq!(v.len(), 3);
                    assert_eq!(v[2], "hi");
                    assert_eq!(keys[0], 2);
                }
                other => panic!("expected String dict (Nullable wrapper stripped on the wire), got {other:?}"),
            }
        }
        other => panic!("expected LowCardinality, got {other:?}"),
    }
}

// =====================================================================
// Phase 10: INSERT path
// =====================================================================

use ch_proto::proto::block::{Block as ProtoBlock, BlockInfo};
use ch_proto::proto::column::{Column, ColumnData, Serialization};

/// Helper: create+drop the named table around `f`.
fn with_table<F: FnOnce(&mut Connection)>(table: &str, ddl: &str, f: F) {
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let _ = conn.query(&format!("DROP TABLE IF EXISTS {table}"));
    conn.query(ddl).unwrap();
    f(&mut conn);
    let _ = conn.query(&format!("DROP TABLE IF EXISTS {table}"));
}

#[test]
fn test_insert_single_block_uint8() {
    require_server();
    with_table(
        "ch_proto_insert_test_u8",
        "CREATE TABLE ch_proto_insert_test_u8 (id UInt8) Engine=Memory",
        |conn| {
            let block = ProtoBlock {
                info: Some(BlockInfo {
                    overflows: false,
                    bucket_number: -1,
                    out_of_order_buckets: Vec::new(),
                }),
                columns: vec![Column {
                    name: "id".to_string(),
                    data_type: "UInt8".to_string(),
                    serialization: Serialization::Default,
                    data: ColumnData::Uint8(vec![10, 20, 30]),
                }],
                rows: 3,
            };
            conn.insert("INSERT INTO ch_proto_insert_test_u8 VALUES", block).unwrap();

            // Verify by SELECT.
            let result = conn.query("SELECT id FROM ch_proto_insert_test_u8 ORDER BY id").unwrap();
            assert_eq!(result.row_count(), 3);
            match &result.rows[0].columns[0].data {
                ColumnData::Uint8(v) => assert_eq!(v, &vec![10, 20, 30]),
                other => panic!("expected Uint8, got {other:?}"),
            }
        },
    );
}

#[test]
fn test_insert_two_columns() {
    require_server();
    with_table(
        "ch_proto_insert_test_two",
        "CREATE TABLE ch_proto_insert_test_two (id UInt32, name String) Engine=Memory",
        |conn| {
            let block = ProtoBlock {
                info: Some(BlockInfo {
                    overflows: false,
                    bucket_number: -1,
                    out_of_order_buckets: Vec::new(),
                }),
                columns: vec![
                    Column {
                        name: "id".to_string(),
                        data_type: "UInt32".to_string(),
                        serialization: Serialization::Default,
                        data: ColumnData::Uint32(vec![1, 2, 3]),
                    },
                    Column {
                        name: "name".to_string(),
                        data_type: "String".to_string(),
                        serialization: Serialization::Default,
                        data: ColumnData::String(vec![
                            "alice".to_string(),
                            "bob".to_string(),
                            "carol".to_string(),
                        ]),
                    },
                ],
                rows: 3,
            };
            conn.insert(
                "INSERT INTO ch_proto_insert_test_two VALUES",
                block,
            )
            .unwrap();

            let result = conn
                .query("SELECT id, name FROM ch_proto_insert_test_two ORDER BY id")
                .unwrap();
            assert_eq!(result.row_count(), 3);
            match &result.rows[0].columns[0].data {
                ColumnData::Uint32(v) => assert_eq!(v, &vec![1u32, 2, 3]),
                other => panic!("expected Uint32, got {other:?}"),
            }
            match &result.rows[0].columns[1].data {
                ColumnData::String(v) => assert_eq!(v, &vec!["alice", "bob", "carol"]),
                other => panic!("expected String, got {other:?}"),
            }
        },
    );
}

#[test]
fn test_insert_empty_block() {
    // A no-op INSERT — no blocks, just the terminator. Server should accept.
    require_server();
    with_table(
        "ch_proto_insert_test_empty",
        "CREATE TABLE ch_proto_insert_test_empty (id UInt8) Engine=Memory",
        |conn| {
            conn.insert_blocks(
                "INSERT INTO ch_proto_insert_test_empty VALUES",
                vec![],
            )
            .unwrap();
            let result = conn
                .query("SELECT count() FROM ch_proto_insert_test_empty")
                .unwrap();
            // count() returns UInt64.
            match &result.rows[0].columns[0].data {
                ColumnData::Uint64(v) => assert_eq!(v[0], 0),
                other => panic!("expected Uint64, got {other:?}"),
            }
        },
    );
}

#[test]
fn test_insert_multiple_blocks() {
    // Streaming INSERT: push multiple blocks.
    require_server();
    with_table(
        "ch_proto_insert_test_multi",
        "CREATE TABLE ch_proto_insert_test_multi (id UInt32) Engine=Memory",
        |conn| {
            let make_block = |values: Vec<u32>| ProtoBlock {
                info: Some(BlockInfo {
                    overflows: false,
                    bucket_number: -1,
                    out_of_order_buckets: Vec::new(),
                }),
                columns: vec![Column {
                    name: "id".to_string(),
                    data_type: "UInt32".to_string(),
                    serialization: Serialization::Default,
                    data: ColumnData::Uint32(values.clone()),
                }],
                rows: values.len(),
            };
            conn.insert_blocks(
                "INSERT INTO ch_proto_insert_test_multi VALUES",
                vec![make_block(vec![1, 2, 3]), make_block(vec![10, 20, 30])],
            )
            .unwrap();

            let result = conn
                .query("SELECT id FROM ch_proto_insert_test_multi ORDER BY id")
                .unwrap();
            assert_eq!(result.row_count(), 6);
            match &result.rows[0].columns[0].data {
                ColumnData::Uint32(v) => {
                    assert_eq!(v, &vec![1u32, 2, 3, 10, 20, 30]);
                }
                other => panic!("expected Uint32, got {other:?}"),
            }
        },
    );
}

#[test]
fn test_insert_rejects_bad_schema() {
    // Schema mismatch: INSERT into UInt8 column with String data — server
    // should respond with Exception.
    require_server();
    with_table(
        "ch_proto_insert_test_bad",
        "CREATE TABLE ch_proto_insert_test_bad (id UInt8) Engine=Memory",
        |conn| {
            let block = ProtoBlock {
                info: Some(BlockInfo {
                    overflows: false,
                    bucket_number: -1,
                    out_of_order_buckets: Vec::new(),
                }),
                columns: vec![Column {
                    name: "id".to_string(),
                    data_type: "String".to_string(), // wrong!
                    serialization: Serialization::Default,
                    data: ColumnData::String(vec!["x".to_string()]),
                }],
                rows: 1,
            };
            let res = conn.insert("INSERT INTO ch_proto_insert_test_bad VALUES", block);
            assert!(res.is_err());
        },
    );
}

// -- v54484: PROGRESS_IN_ASYNC_INSERT --

#[test]
fn test_v54484_negotiated() {
    // The current server target advertises protocol 54484; the client
    // declares the same as its max, so the negotiated version is 54484.
    require_server();
    let conn = Connection::connect(ADDR, None, None, None).unwrap();
    assert_eq!(
        conn.protocol(),
        54484,
        "expected to negotiate v54484 (PROGRESS_IN_ASYNC_INSERT) with the server"
    );
}

#[test]
fn test_insert_async_reports_progress() {
    // v54484: on an async INSERT the server flushes the batch and then sends
    // an extra Progress packet carrying the written rows/bytes before
    // EndOfStream. The client must tolerate (and here, capture) it.
    require_server();
    with_table(
        "ch_proto_async_insert",
        "CREATE TABLE ch_proto_async_insert (id UInt32) Engine=MergeTree ORDER BY id",
        |conn| {
            // Force the async path and block until the flush completes so the
            // trailing Progress is emitted synchronously within the INSERT.
            conn.query("SET async_insert = 1").unwrap();
            conn.query("SET wait_for_async_insert = 1").unwrap();

            let block = ProtoBlock {
                info: Some(BlockInfo {
                    overflows: false,
                    bucket_number: -1,
                    out_of_order_buckets: Vec::new(),
                }),
                columns: vec![Column {
                    name: "id".to_string(),
                    data_type: "UInt32".to_string(),
                    serialization: Serialization::Default,
                    data: ColumnData::Uint32(vec![100, 200]),
                }],
                rows: 2,
            };
            conn.insert("INSERT INTO ch_proto_async_insert VALUES", block)
                .unwrap();

            // v54484: the trailing async-insert Progress packet must have
            // been received and fully parsed (all feature-gated fields at
            // v54484), proving the stream stayed aligned through it. The
            // server reports the written-row counters via the accompanying
            // ProfileEvents, not this Progress (the pipeline is reset before
            // the write counts are folded in), so the increment carries the
            // elapsed time — that's what we assert here.
            let p = conn
                .insert_progress()
                .expect("async INSERT at v54484 should yield a trailing Progress packet");
            assert!(
                matches!(p.elapsed_ns, Some(n) if n > 0),
                "async-insert Progress should carry a non-zero elapsed_ns, got {p:?}"
            );

            // And the rows actually landed.
            let result = conn
                .query("SELECT count() FROM ch_proto_async_insert")
                .unwrap();
            match &result.rows[0].columns[0].data {
                ColumnData::Uint64(v) => assert_eq!(v[0], 2),
                other => panic!("expected Uint64 count, got {other:?}"),
            }
        },
    );
}

// -- Variant(T1, T2, ...) (Problem 38) --

#[test]
fn test_variant_select() {
    // arrayJoin over a 3-element array yields one block of 3 rows in array
    // order: 42 (UInt64), "hi" (String), NULL. The server canonicalises the
    // type to Variant(String, UInt64), so String = discriminator 0,
    // UInt64 = 1, NULL = 255.
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query(
            "SELECT arrayJoin([\
                CAST(42::UInt64, 'Variant(String, UInt64)'), \
                CAST('hi'::String, 'Variant(String, UInt64)'), \
                CAST(NULL, 'Variant(String, UInt64)')]) AS v",
        )
        .unwrap();
    assert_eq!(result.row_count(), 3);

    let col = &result.rows[0].columns[0];
    assert!(
        col.data_type.starts_with("Variant"),
        "expected a Variant column, got type {}",
        col.data_type
    );
    match &col.data {
        ColumnData::Variant {
            discriminators,
            columns,
            ..
        } => {
            assert_eq!(discriminators, &vec![1u8, 0, 255]);
            match &columns[0] {
                ColumnData::String(v) => assert_eq!(v, &vec!["hi".to_string()]),
                other => panic!("expected String run, got {other:?}"),
            }
            match &columns[1] {
                ColumnData::Uint64(v) => assert_eq!(v, &vec![42u64]),
                other => panic!("expected UInt64 run, got {other:?}"),
            }
        }
        other => panic!("expected Variant, got {other:?}"),
    }

    // End-to-end TSV rendering: active value per row, \N for NULL.
    let mut out = Vec::new();
    for row in 0..col.data.row_count() {
        ch_proto::tsv::write_value(&mut out, &col.data, row).unwrap();
        out.push(b'\n');
    }
    assert_eq!(String::from_utf8(out).unwrap(), "42\nhi\n\\N\n");
}

#[test]
fn test_variant_with_array_element_and_offsets() {
    // A composite variant element (Array) plus a repeated discriminator,
    // exercising the dense per-type reconstruction and offsets. Canonical
    // order sorts "Array(UInt8)" before "String": Array = 0, String = 1.
    // Rows: [1,2,3] (Array), "x" (String), [9] (Array) → discriminators
    // [0, 1, 0], Array run [[1,2,3],[9]], offsets [0, 0, 1].
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query(
            "SELECT arrayJoin([\
                CAST([1,2,3]::Array(UInt8), 'Variant(Array(UInt8), String)'), \
                CAST('x'::String, 'Variant(Array(UInt8), String)'), \
                CAST([9]::Array(UInt8), 'Variant(Array(UInt8), String)')]) AS v",
        )
        .unwrap();
    assert_eq!(result.row_count(), 3);

    match &result.rows[0].columns[0].data {
        ColumnData::Variant {
            discriminators,
            offsets,
            columns,
        } => {
            assert_eq!(discriminators, &vec![0u8, 1, 0]);
            assert_eq!(offsets, &vec![0u64, 0, 1]);
            // columns[0] is Array(UInt8) holding two arrays: [1,2,3] and [9].
            match &columns[0] {
                ColumnData::Array { inner, offsets } => {
                    assert_eq!(offsets, &vec![3u64, 4]);
                    match inner.as_ref() {
                        ColumnData::Uint8(v) => assert_eq!(v, &vec![1u8, 2, 3, 9]),
                        other => panic!("expected UInt8 array inner, got {other:?}"),
                    }
                }
                other => panic!("expected Array run, got {other:?}"),
            }
            match &columns[1] {
                ColumnData::String(v) => assert_eq!(v, &vec!["x".to_string()]),
                other => panic!("expected String run, got {other:?}"),
            }
        }
        other => panic!("expected Variant, got {other:?}"),
    }
}

// -- Dynamic (Problem 39) --

#[test]
fn test_dynamic_select() {
    // arrayJoin over a 3-element array yields one block of 3 rows in array
    // order: 42 (UInt64), "hi" (String), NULL. The runtime type set is
    // carried in the Dynamic state prefix; the NULL discriminator is the
    // type count (one past the last type).
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query(
            "SELECT arrayJoin([\
                42::UInt64::Dynamic, \
                'hi'::String::Dynamic, \
                CAST(NULL, 'Dynamic')]) AS d",
        )
        .unwrap();
    assert_eq!(result.row_count(), 3);

    let col = &result.rows[0].columns[0];
    assert!(
        col.data_type.starts_with("Dynamic"),
        "expected a Dynamic column, got type {}",
        col.data_type
    );
    match &col.data {
        ColumnData::Dynamic {
            type_names,
            discriminators,
            columns,
            ..
        } => {
            let null = type_names.len() as u64;
            // Map each row to its rendered value via type_names, independent
            // of the server's type ordering.
            let uint_idx = type_names.iter().position(|t| t == "UInt64").expect("UInt64 type");
            let str_idx = type_names.iter().position(|t| t == "String").expect("String type");
            assert_eq!(discriminators.len(), 3);
            assert_eq!(discriminators[0], uint_idx as u64);
            assert_eq!(discriminators[1], str_idx as u64);
            assert_eq!(discriminators[2], null);
            match &columns[uint_idx] {
                ColumnData::Uint64(v) => assert_eq!(v, &vec![42u64]),
                other => panic!("expected UInt64 run, got {other:?}"),
            }
            match &columns[str_idx] {
                ColumnData::String(v) => assert_eq!(v, &vec!["hi".to_string()]),
                other => panic!("expected String run, got {other:?}"),
            }
        }
        other => panic!("expected Dynamic, got {other:?}"),
    }

    // End-to-end TSV rendering.
    let mut out = Vec::new();
    for row in 0..col.data.row_count() {
        ch_proto::tsv::write_value(&mut out, &col.data, row).unwrap();
        out.push(b'\n');
    }
    assert_eq!(String::from_utf8(out).unwrap(), "42\nhi\n\\N\n");
}

#[test]
fn test_dynamic_repeated_type_offsets() {
    // Two UInt64 values plus one String exercise the per-type dense runs and
    // offsets: the UInt64 run holds [1, 2] at offsets 0 and 1.
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let result = conn
        .query(
            "SELECT arrayJoin([\
                1::UInt64::Dynamic, \
                2::UInt64::Dynamic, \
                'x'::String::Dynamic]) AS d",
        )
        .unwrap();
    assert_eq!(result.row_count(), 3);

    match &result.rows[0].columns[0].data {
        ColumnData::Dynamic {
            type_names,
            discriminators,
            offsets,
            columns,
        } => {
            let uint_idx = type_names.iter().position(|t| t == "UInt64").unwrap() as u64;
            // Two UInt64 rows then one String row.
            assert_eq!(discriminators[0], uint_idx);
            assert_eq!(discriminators[1], uint_idx);
            assert_eq!(offsets[0], 0);
            assert_eq!(offsets[1], 1);
            match &columns[uint_idx as usize] {
                ColumnData::Uint64(v) => assert_eq!(v, &vec![1u64, 2]),
                other => panic!("expected UInt64 run, got {other:?}"),
            }
        }
        other => panic!("expected Dynamic, got {other:?}"),
    }
}

// -- JSON Tier 2 (FLATTENED Object) (Problem 41, partial) --

#[test]
fn test_json_tier2_flattened_select() {
    // Disabling the Tier 1 String fallback makes the server emit the
    // FLATTENED Object (version 3) serialization. A JSON value with two
    // scalar paths comes back as two dynamic-path Dynamic columns.
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let opts = QueryOptions::new()
        .with_setting("output_format_native_write_json_as_string", "0");
    let result = conn
        .query_with(
            "SELECT '{\"a\": 1, \"b\": \"hi\"}'::JSON AS j",
            opts,
        )
        .unwrap();
    assert_eq!(result.row_count(), 1);

    let col = &result.rows[0].columns[0];
    match &col.data {
        ColumnData::JsonObject {
            rows,
            dynamic_paths,
            ..
        } => {
            assert_eq!(*rows, 1);
            // Paths "a" and "b" are runtime-discovered (dynamic).
            let a = dynamic_paths
                .iter()
                .find(|(p, _)| p == "a")
                .expect("path a");
            let b = dynamic_paths
                .iter()
                .find(|(p, _)| p == "b")
                .expect("path b");
            // Each is a Dynamic with one non-NULL row.
            for (path, col) in [a, b] {
                match col {
                    ColumnData::Dynamic { discriminators, columns, .. } => {
                        assert_eq!(discriminators.len(), 1, "path {path}");
                        // discriminator is a real type index (not NULL).
                        assert!((discriminators[0] as usize) < columns.len(), "path {path} not null");
                    }
                    other => panic!("expected Dynamic at {path}, got {other:?}"),
                }
            }
        }
        other => panic!("expected JsonObject, got {other:?}"),
    }

    // Best-effort JSON rendering contains both paths.
    let mut out = Vec::new();
    ch_proto::tsv::write_value(&mut out, &col.data, 0).unwrap();
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("\"a\":"), "rendered {s}");
    assert!(s.contains("\"b\":"), "rendered {s}");
}

// -- Compression connection integration (Problems 42/43) --

#[test]
fn test_select_compressed() {
    // With compression on, the server wraps result block bodies in the
    // compression frame; the client must decompress them. 1000 rows is large
    // enough that the body actually compresses.
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let opts = QueryOptions::new().with_compression(true);
    let result = conn
        .query_with("SELECT number FROM numbers(1000) ORDER BY number", opts)
        .unwrap();
    assert_eq!(result.row_count(), 1000);
    // First block's first value is 0; spot-check a known value.
    match &result.rows[0].columns[0].data {
        ColumnData::Uint64(v) => assert_eq!(v[0], 0),
        other => panic!("expected Uint64, got {other:?}"),
    }
    // Sum all values across blocks to confirm none were lost/corrupted.
    let mut sum: u64 = 0;
    for block in &result.rows {
        if let ColumnData::Uint64(v) = &block.columns[0].data {
            sum += v.iter().sum::<u64>();
        }
    }
    assert_eq!(sum, (0..1000).sum::<u64>());
}

#[test]
fn test_select_compressed_matches_uncompressed() {
    // The same query with and without compression yields identical data.
    require_server();
    let mut conn = Connection::connect(ADDR, None, None, None).unwrap();
    let sql = "SELECT toString(number) AS s FROM numbers(500) ORDER BY number";
    let plain = conn.query(sql).unwrap();
    let compressed = conn
        .query_with(sql, QueryOptions::new().with_compression(true))
        .unwrap();
    assert_eq!(plain.row_count(), compressed.row_count());
    // Compare the rendered TSV of every row.
    let render = |r: &ch_proto::query_result::QueryResult| {
        let mut out = Vec::new();
        for block in &r.rows {
            for row in 0..block.columns[0].data.row_count() {
                ch_proto::tsv::write_value(&mut out, &block.columns[0].data, row).unwrap();
                out.push(b'\n');
            }
        }
        out
    };
    assert_eq!(render(&plain), render(&compressed));
}

#[test]
fn test_insert_compressed_is_rejected() {
    // Compressed INSERT (client→server) is deferred — the server's parallel
    // block-marshalling / ColumnBLOB path (v54478) isn't handled. Requesting
    // it must fail cleanly rather than hang or corrupt the stream.
    require_server();
    with_table(
        "ch_proto_insert_compressed",
        "CREATE TABLE ch_proto_insert_compressed (id UInt32) Engine=Memory",
        |conn| {
            let block = ProtoBlock {
                info: Some(BlockInfo {
                    overflows: false,
                    bucket_number: -1,
                    out_of_order_buckets: Vec::new(),
                }),
                columns: vec![Column {
                    name: "id".to_string(),
                    data_type: "UInt32".to_string(),
                    serialization: Serialization::Default,
                    data: ColumnData::Uint32(vec![10]),
                }],
                rows: 1,
            };
            let err = conn
                .insert_blocks_with(
                    "INSERT INTO ch_proto_insert_compressed VALUES",
                    vec![block],
                    QueryOptions::new().with_compression(true),
                )
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::Unsupported);

            // The connection is still usable for a normal (uncompressed) query.
            let result = conn
                .query("SELECT count() FROM ch_proto_insert_compressed")
                .unwrap();
            match &result.rows[0].columns[0].data {
                ColumnData::Uint64(v) => assert_eq!(v[0], 0),
                other => panic!("expected Uint64, got {other:?}"),
            }
        },
    );
}
