//! Roundtrip composite types, LowCardinality, JSON, and Decimal — the
//! "structured" half of the type system.
//!
//! Run with a ClickHouse server on 127.0.0.1:9000:
//!
//! ```sh
//! cargo run --example catalog
//! ```
//!
//! Creates a temporary `catalog_demo` table modeling a product catalog:
//!
//! - `Array(T)`, `Tuple(...)`, `Map(K, V)` — composites
//! - `LowCardinality(String)` — versioned dictionary-encoded
//! - `JSON` — Tier 1 (String fallback; the client auto-injects the
//!   `output_format_native_write_json_as_string=1` setting)
//! - `Decimal(P, S)` at two widths (Decimal32, Decimal64)
//! - `FixedString(N)`, `Nullable(T)` — basic parameterized
//!
//! Composites in this client compose freely — `Array(Tuple(...))`,
//! `Map(String, Array(...))`, etc. work out of the box.

use ch_proto::client::Connection;
use ch_proto::proto::block::{Block, BlockInfo};
use ch_proto::proto::column::{Column, ColumnData, Serialization};
use std::io::Result;

const ADDR: &str = "127.0.0.1:9000";
const TABLE: &str = "catalog_demo";

fn col(name: &str, data_type: &str, data: ColumnData) -> Column {
    Column {
        name: name.to_string(),
        data_type: data_type.to_string(),
        serialization: Serialization::Default,
        data,
    }
}

fn main() -> Result<()> {
    let mut conn = Connection::connect(ADDR, None, None, None)?;

    // -- 1. Schema --
    conn.query(&format!("DROP TABLE IF EXISTS {TABLE}"))?;
    conn.query(&format!(
        "CREATE TABLE {TABLE} (
            product_id  UInt32,
            sku         FixedString(8),
            name        String,
            category    LowCardinality(String),
            tags        Array(String),
            price       Decimal(9, 2),
            ext_price   Decimal(18, 4),
            dimensions  Tuple(UInt16, UInt16, UInt16),
            attrs       Map(String, String),
            opt_note    Nullable(String),
            metadata    JSON
        ) Engine = Memory"
    ))?;

    // -- 2. Build a 3-row block. --
    //
    // Composites need offsets/keys to be consistent — see SPEC §8.3:
    //   Array:  inner.row_count() == offsets.last(); offsets are cumulative.
    //   Tuple:  every element column has the same row_count.
    //   Map:    keys.row_count() == values.row_count() == offsets.last().
    //
    // For 3 rows of `tags`:
    //   row 0: ["new", "sale"]      (2 elements)
    //   row 1: []                   (0 elements)
    //   row 2: ["clearance"]        (1 element)
    // -> offsets = [2, 2, 3], inner = ["new", "sale", "clearance"]
    let block = Block {
        info: Some(BlockInfo {
            overflows: false,
            bucket_number: -1,
        }),
        columns: vec![
            col(
                "product_id",
                "UInt32",
                ColumnData::Uint32(vec![1001, 1002, 1003]),
            ),
            // FixedString(8) — exactly 8 bytes per row, total 24.
            col(
                "sku",
                "FixedString(8)",
                ColumnData::FixedString {
                    n: 8,
                    data: b"WIDGET01GIZMO-02SOLDOUT3".to_vec(),
                },
            ),
            col(
                "name",
                "String",
                ColumnData::String(vec![
                    "Widget".to_string(),
                    "Gizmo".to_string(),
                    "Sold Out Item".to_string(),
                ]),
            ),
            // LowCardinality dispatches by inner type — pass a String dict
            // and indices. By convention dict[0] is an empty placeholder.
            // Our 3 rows reference dict[1], dict[2], dict[1] respectively.
            col(
                "category",
                "LowCardinality(String)",
                ColumnData::LowCardinality {
                    dict: Box::new(ColumnData::String(vec![
                        "".to_string(),        // placeholder
                        "tools".to_string(),   // dict[1]
                        "kitchen".to_string(), // dict[2]
                    ])),
                    keys: vec![1, 2, 1],
                    key_width: 1, // UInt8 keys (dict has 3 entries — fits)
                },
            ),
            // Array(String) — see comment above.
            col(
                "tags",
                "Array(String)",
                ColumnData::Array {
                    inner: Box::new(ColumnData::String(vec![
                        "new".to_string(),
                        "sale".to_string(),
                        "clearance".to_string(),
                    ])),
                    offsets: vec![2, 2, 3],
                },
            ),
            // Decimal(9, 2): scale 2, value × 100. 12.99 → 1299.
            col(
                "price",
                "Decimal(9, 2)",
                ColumnData::Decimal32 {
                    scale: 2,
                    values: vec![1299, 4500, 0],
                },
            ),
            // Decimal(18, 4): scale 4, value × 10000.
            col(
                "ext_price",
                "Decimal(18, 4)",
                ColumnData::Decimal64 {
                    scale: 4,
                    values: vec![129_900, 450_000, 0],
                },
            ),
            // Tuple(UInt16, UInt16, UInt16) — every element has 3 rows.
            col(
                "dimensions",
                "Tuple(UInt16, UInt16, UInt16)",
                ColumnData::Tuple(vec![
                    ColumnData::Uint16(vec![10, 20, 5]), // length
                    ColumnData::Uint16(vec![5, 10, 5]),  // width
                    ColumnData::Uint16(vec![3, 5, 1]),   // height
                ]),
            ),
            // Map(String, String) — wire-equivalent to Array(Tuple(K, V)).
            //   row 0: {"color": "red", "weight": "1kg"}     (2 pairs)
            //   row 1: {"color": "blue"}                      (1 pair)
            //   row 2: {}                                     (0 pairs)
            // -> offsets = [2, 3, 3], total 3 K-V pairs.
            col(
                "attrs",
                "Map(String, String)",
                ColumnData::Map {
                    keys: Box::new(ColumnData::String(vec![
                        "color".to_string(),
                        "weight".to_string(),
                        "color".to_string(),
                    ])),
                    values: Box::new(ColumnData::String(vec![
                        "red".to_string(),
                        "1kg".to_string(),
                        "blue".to_string(),
                    ])),
                    offsets: vec![2, 3, 3],
                },
            ),
            // Nullable(String) — null map alongside the value column. Inner
            // values for null rows are placeholders.
            col(
                "opt_note",
                "Nullable(String)",
                ColumnData::Nullable {
                    inner: Box::new(ColumnData::String(vec![
                        "ships fast".to_string(),
                        "".to_string(),
                        "discontinued".to_string(),
                    ])),
                    nulls: vec![0, 1, 0],
                },
            ),
            // JSON — Tier 1 (String fallback). Client auto-injects the
            // server setting; we just hand over JSON text per row.
            col(
                "metadata",
                "JSON",
                ColumnData::Json(vec![
                    r#"{"vendor":"acme","sku_alt":"W-1"}"#.to_string(),
                    r#"{"vendor":"globex"}"#.to_string(),
                    r#"{}"#.to_string(),
                ]),
            ),
        ],
        rows: 3,
    };

    // -- 3. INSERT --
    conn.insert(&format!("INSERT INTO {TABLE} VALUES"), block)?;
    println!("inserted 3 products");

    // -- 4. SELECT --
    let result = conn.query(&format!(
        "SELECT product_id, sku, name, category, tags, price, ext_price,
                dimensions, attrs, opt_note, metadata
         FROM {TABLE} ORDER BY product_id"
    ))?;

    println!("\nSELECT returned {} row(s)", result.row_count());
    for (i, b) in result.rows.iter().enumerate() {
        println!("\n-- block {i} --");
        for c in &b.columns {
            let summary = format!("{:?}", c.data);
            let trimmed = if summary.len() > 220 {
                format!("{}…", &summary[..220])
            } else {
                summary
            };
            println!("  {:<12} {:<32} {trimmed}", c.name, c.data_type);
        }
    }

    // Demonstrate that decoded composites really are inspectable in Rust:
    // unpack the first row's tags array.
    if let Some(b) = result.rows.first() {
        if let Some(tags_col) = b.columns.iter().find(|c| c.name == "tags") {
            if let ColumnData::Array { inner, offsets } = &tags_col.data {
                if let ColumnData::String(strings) = inner.as_ref() {
                    let row0_start = 0;
                    let row0_end = offsets[0] as usize;
                    let row0_tags: Vec<&str> = strings[row0_start..row0_end]
                        .iter()
                        .map(String::as_str)
                        .collect();
                    println!("\nrow 0 tags reconstructed: {row0_tags:?}");
                }
            }
        }
    }

    // -- 5. Cleanup --
    // conn.query(&format!("DROP TABLE {TABLE}"))?;
    // println!("\ndropped {TABLE}");
    Ok(())
}
