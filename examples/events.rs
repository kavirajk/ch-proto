//! Roundtrip every flat (non-composite) data type the client supports.
//!
//! Run with a ClickHouse server listening on 127.0.0.1:9000 (the project's
//! `make up` brings one up via docker-compose):
//!
//! ```sh
//! cargo run --example events
//! ```
//!
//! The example creates a temporary `events_demo` table with one column per
//! supported flat type, INSERTs three rows constructed in Rust, SELECTs them
//! back, prints the decoded values, and drops the table.

use ch_proto::client::Connection;
use ch_proto::proto::block::{Block, BlockInfo};
use ch_proto::proto::column::{Column, ColumnData, Serialization};
use std::io::Result;
use uuid::Uuid;

const ADDR: &str = "127.0.0.1:9000";
const TABLE: &str = "events_demo";

/// Concise constructor — every Column in this example uses `Default`
/// serialization, so wrap the boilerplate.
fn col(name: &str, data_type: &str, data: ColumnData) -> Column {
    Column {
        name: name.to_string(),
        data_type: data_type.to_string(),
        serialization: Serialization::Default,
        data,
    }
}

fn ipv6_octets(s: &str) -> [u8; 16] {
    s.parse::<std::net::Ipv6Addr>().unwrap().octets()
}

fn main() -> Result<()> {
    let mut conn = Connection::connect(ADDR, None, None, None)?;

    // -- 1. Create the schema --
    conn.query(&format!("DROP TABLE IF EXISTS {TABLE}"))?;
    conn.query(&format!(
        "CREATE TABLE {TABLE} (
            event_id      UUID,
            created_at    DateTime,
            ts            DateTime64(3, 'UTC'),
            birthday      Date,
            signup_date   Date32,
            server_ip     IPv4,
            client_ip     IPv6,
            status        Enum8('ok' = 1, 'fail' = 2),
            log_level     Enum16('debug' = 10, 'info' = 20, 'warn' = 300, 'error' = 500),
            retry_count   UInt8,
            user_age      Int16,
            sequence      UInt32,
            duration_ns   Int64,
            request_hash  UInt128,
            large_signed  Int128,
            cpu_pct       Float32,
            load_avg      Float64,
            success       Bool,
            message       String,
            region        FixedString(8),
            error_msg     Nullable(String)
        ) Engine = Memory"
    ))?;

    // -- 2. Build a Block holding 3 rows. Every Vec must have length 3. --
    //
    // For UUID we use the `uuid` crate; for IPv4 the value is the canonical
    // 32-bit integer (192.168.1.10 = 0xC0A8010A); IPv6 is the 16 network-order
    // bytes (use the std parser for ergonomics).
    let block = Block {
        info: Some(BlockInfo {
            overflows: false,
            bucket_number: -1,
            out_of_order_buckets: Vec::new(),
        }),
        columns: vec![
            col(
                "event_id",
                "UUID",
                ColumnData::Uuid(vec![
                    Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
                    Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
                    Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
                ]),
            ),
            // DateTime = Unix seconds. 1700000000 = 2023-11-14 22:13:20 UTC.
            col(
                "created_at",
                "DateTime",
                ColumnData::DateTime(vec![1_700_000_000, 1_700_000_001, 1_700_000_002]),
            ),
            // DateTime64(3) = Unix milliseconds.
            col(
                "ts",
                "DateTime64(3, 'UTC')",
                ColumnData::DateTime64 {
                    scale: 3,
                    values: vec![1_700_000_000_123, 1_700_000_000_456, 1_700_000_000_789],
                },
            ),
            // Date = days since 1970-01-01. 19737 = 2024-01-15.
            col(
                "birthday",
                "Date",
                ColumnData::Date(vec![19_737, 19_738, 19_739]),
            ),
            // Date32 = signed days, can go pre-1970. -25567 = 1900-01-01.
            col(
                "signup_date",
                "Date32",
                ColumnData::Date32(vec![-25567, 0, 19_737]),
            ),
            // IPv4 stored as the canonical 32-bit integer (network order
            // packed into a u32 — the wire encoding will LE this).
            col(
                "server_ip",
                "IPv4",
                ColumnData::Ipv4(vec![0xC0A8010A, 0x7F000001, 0x08080808]),
            ),
            col(
                "client_ip",
                "IPv6",
                ColumnData::Ipv6(vec![
                    ipv6_octets("2001:db8::1"),
                    ipv6_octets("::1"),
                    ipv6_octets("fe80::1"),
                ]),
            ),
            // Enum8 carries Int8 on the wire — we pass the underlying integer
            // values; the SELECT side resolves them to labels.
            col(
                "status",
                "Enum8('ok' = 1, 'fail' = 2)",
                ColumnData::Int8(vec![1, 2, 1]),
            ),
            // Enum16 carries Int16.
            col(
                "log_level",
                "Enum16('debug' = 10, 'info' = 20, 'warn' = 300, 'error' = 500)",
                ColumnData::Enum16 {
                    values: vec![20, 300, 500],
                    names: vec![
                        (10, "debug".into()),
                        (20, "info".into()),
                        (300, "warn".into()),
                        (500, "error".into()),
                    ],
                },
            ),
            col("retry_count", "UInt8", ColumnData::Uint8(vec![0, 3, 7])),
            col("user_age", "Int16", ColumnData::Int16(vec![25, -1, 10_000])),
            col("sequence", "UInt32", ColumnData::Uint32(vec![1, 2, 3])),
            col(
                "duration_ns",
                "Int64",
                ColumnData::Int64(vec![1_200_000, 5_500_000, 12_000_000]),
            ),
            // u128 / i128 are native in Rust — pass them straight through.
            col(
                "request_hash",
                "UInt128",
                ColumnData::Uint128(vec![
                    0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210u128,
                    0x1111_2222_3333_4444_5555_6666_7777_8888u128,
                    u128::MAX,
                ]),
            ),
            col(
                "large_signed",
                "Int128",
                ColumnData::Int128(vec![i128::MIN, 0, i128::MAX]),
            ),
            col(
                "cpu_pct",
                "Float32",
                ColumnData::Float32(vec![12.5, 75.0, 99.9]),
            ),
            col(
                "load_avg",
                "Float64",
                ColumnData::Float64(vec![0.42, 1.5, 2.71828]),
            ),
            col("success", "Bool", ColumnData::Bool(vec![true, false, true])),
            col(
                "message",
                "String",
                ColumnData::String(vec![
                    "ok".to_string(),
                    "timeout".to_string(),
                    "✓ retried".to_string(), // unicode round-trip
                ]),
            ),
            // FixedString(8) — exactly 8 bytes per row, NUL-padded by the
            // server if shorter. Pass 24 bytes total for 3 rows.
            col(
                "region",
                "FixedString(8)",
                ColumnData::FixedString {
                    n: 8,
                    data: b"us-east1eu-west1ap-south".to_vec(),
                },
            ),
            // Nullable: nulls = 1 byte per row (0 = present, 1 = null). The
            // inner Vec length must equal nulls.len() — null rows still need a
            // placeholder value.
            col(
                "error_msg",
                "Nullable(String)",
                ColumnData::Nullable {
                    inner: Box::new(ColumnData::String(vec![
                        "".to_string(),
                        "deadline exceeded".to_string(),
                        "".to_string(),
                    ])),
                    nulls: vec![1, 0, 1],
                },
            ),
        ],
        rows: 3,
    };

    // -- 3. INSERT --
    conn.insert(&format!("INSERT INTO {TABLE} VALUES"), block)?;
    println!("inserted 3 rows");

    // -- 4. SELECT --
    let sql = format!(
        "SELECT
            event_id, created_at, ts, birthday, signup_date,
            IPv4NumToString(server_ip) AS server_ip,
            IPv6NumToString(client_ip) AS client_ip,
            status, log_level, retry_count, user_age, sequence, duration_ns,
            request_hash, large_signed, cpu_pct, load_avg, success,
            message, region, error_msg
         FROM {TABLE} ORDER BY sequence"
    );
    // Note: server_ip/client_ip cast via `IPv4/v6NumToString` returns String —
    // demonstrates that the protocol round-trip is fine even when the SELECT
    // post-processes columns. To inspect raw IP bytes, query without the
    // conversion and you get back ColumnData::Ipv4 / ColumnData::Ipv6.
    let result = conn.query(&sql)?;

    println!("\nSELECT returned {} row(s)", result.row_count());
    for (i, b) in result.rows.iter().enumerate() {
        println!("\n-- block {i} --");
        for c in &b.columns {
            // Truncate noisy ColumnData::String / FixedString outputs.
            let summary = format!("{:?}", c.data);
            let trimmed = if summary.len() > 200 {
                format!("{}…", &summary[..200])
            } else {
                summary
            };
            println!("  {:<14} {:<48} {trimmed}", c.name, c.data_type);
        }
    }

    // -- 5. Cleanup --
    // conn.query(&format!("DROP TABLE {TABLE}"))?;
    // println!("\ndropped {TABLE}");
    Ok(())
}
