//! Benchmarks for composite column types.
//!
//! Focus areas:
//! - `Nullable(T)` encode/decode vs. bare inner type (overhead of null-map)
//! - `Array(T)` encode/decode across different row-count / element-count ratios
//! - Nested composites (`Array(Array(T))`, `Array(Nullable(T))`) to stress recursion

use ch_proto::proto::column::{Column, ColumnData, Serialization};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::io::Cursor;

const PROTOCOL: u32 = 54459;

fn encode_col(col: &Column, buf: &mut Vec<u8>) {
    buf.clear();
    col.encode(buf, PROTOCOL).unwrap();
}

fn decode_col(buf: &[u8], rows: usize) -> Column {
    let mut cursor = Cursor::new(buf);
    Column::decode(&mut cursor, rows, PROTOCOL).unwrap()
}

// ---------- Nullable ----------

fn bench_nullable(c: &mut Criterion) {
    let mut group = c.benchmark_group("nullable_uint32");

    for rows in [10usize, 1_000, 100_000].iter() {
        // Half the rows are null for realistic ratio.
        let values: Vec<u32> = (0..*rows as u32).collect();
        let nulls: Vec<u8> = (0..*rows).map(|i| (i % 2) as u8).collect();

        let col = Column {
            name: "x".to_string(),
            data_type: "Nullable(UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nullable {
                inner: Box::new(ColumnData::Uint32(values)),
                nulls,
            },
        };

        let mut buf = Vec::with_capacity(16 + rows * 5);
        col.encode(&mut buf, PROTOCOL).unwrap();
        let encoded = buf.clone();

        group.throughput(Throughput::Elements(*rows as u64));
        group.bench_with_input(
            BenchmarkId::new("encode", rows),
            rows,
            |b, _| b.iter(|| encode_col(&col, &mut buf)),
        );
        group.bench_with_input(
            BenchmarkId::new("decode", rows),
            rows,
            |b, n| b.iter(|| decode_col(&encoded, *n)),
        );
    }

    group.finish();
}

// ---------- Array ----------

fn bench_array(c: &mut Criterion) {
    let mut group = c.benchmark_group("array_uint32");

    // For each row count, arrays of average length 10.
    for rows in [10usize, 1_000, 10_000].iter() {
        let avg_len = 10u64;
        let total_elements = *rows as u64 * avg_len;
        let values: Vec<u32> = (0..total_elements as u32).collect();
        let offsets: Vec<u64> = (1..=*rows as u64).map(|i| i * avg_len).collect();

        let col = Column {
            name: "arr".to_string(),
            data_type: "Array(UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Array {
                inner: Box::new(ColumnData::Uint32(values)),
                offsets,
            },
        };

        let mut buf = Vec::with_capacity(32 + (*rows as usize) * 8 + (total_elements as usize) * 4);
        col.encode(&mut buf, PROTOCOL).unwrap();
        let encoded = buf.clone();

        group.throughput(Throughput::Elements(total_elements));
        group.bench_with_input(
            BenchmarkId::new("encode", rows),
            rows,
            |b, _| b.iter(|| encode_col(&col, &mut buf)),
        );
        group.bench_with_input(
            BenchmarkId::new("decode", rows),
            rows,
            |b, n| b.iter(|| decode_col(&encoded, *n)),
        );
    }

    group.finish();
}

// ---------- Nested: Array(Nullable(UInt32)) ----------

fn bench_array_nullable(c: &mut Criterion) {
    let mut group = c.benchmark_group("array_nullable_uint32");

    for rows in [10usize, 1_000, 10_000].iter() {
        let avg_len = 10u64;
        let total_elements = *rows as u64 * avg_len;

        let values: Vec<u32> = (0..total_elements as u32).collect();
        let nulls: Vec<u8> = (0..total_elements).map(|i| (i % 3 == 0) as u8).collect();
        let offsets: Vec<u64> = (1..=*rows as u64).map(|i| i * avg_len).collect();

        let col = Column {
            name: "arr".to_string(),
            data_type: "Array(Nullable(UInt32))".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Array {
                inner: Box::new(ColumnData::Nullable {
                    inner: Box::new(ColumnData::Uint32(values)),
                    nulls,
                }),
                offsets,
            },
        };

        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let encoded = buf.clone();

        group.throughput(Throughput::Elements(total_elements));
        group.bench_with_input(
            BenchmarkId::new("encode", rows),
            rows,
            |b, _| b.iter(|| encode_col(&col, &mut buf)),
        );
        group.bench_with_input(
            BenchmarkId::new("decode", rows),
            rows,
            |b, n| b.iter(|| decode_col(&encoded, *n)),
        );
    }

    group.finish();
}

// ---------- row_count() hot-path ----------

fn bench_row_count(c: &mut Criterion) {
    // row_count is called by validate() on encode; it should be O(1).
    let flat = ColumnData::Uint32((0..10_000u32).collect());
    let nullable = ColumnData::Nullable {
        inner: Box::new(ColumnData::Uint32((0..10_000u32).collect())),
        nulls: vec![0u8; 10_000],
    };
    let array = ColumnData::Array {
        inner: Box::new(ColumnData::Uint32((0..100_000u32).collect())),
        offsets: (0..10_000u64).collect(),
    };

    c.bench_function("row_count_flat", |b| b.iter(|| flat.row_count()));
    c.bench_function("row_count_nullable", |b| b.iter(|| nullable.row_count()));
    c.bench_function("row_count_array", |b| b.iter(|| array.row_count()));
}

criterion_group!(
    benches,
    bench_nullable,
    bench_array,
    bench_array_nullable,
    bench_row_count,
);
criterion_main!(benches);
