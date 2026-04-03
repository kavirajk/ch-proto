use ch_proto::{ProtoRead, ProtoWrite};
use criterion::{Criterion, criterion_group, criterion_main};
use std::io::Cursor;

fn encode(x: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.write_varuint(x).unwrap();
    buf
}

fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode");

    let inputs: &[(&str, u64)] = &[
        ("1_byte_val_42", 42),
        ("2_byte_val_300", 300),
        ("3_byte_val_100000", 100_000),
        ("5_byte_val_4billion", 4_000_000_000),
        ("8_byte_val_2pow55", 1u64 << 55),
        ("9_byte_val_max63bit", u64::MAX >> 1),
        ("10_byte_val_u64max", u64::MAX),
        ("zero", 0),
    ];

    for (name, val) in inputs {
        let val = *val;
        group.bench_function(*name, |b| {
            b.iter(|| {
                let mut buf = Vec::with_capacity(10);
                buf.write_varuint(std::hint::black_box(val)).unwrap();
                buf
            })
        });
    }

    group.finish();
}

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode");

    let inputs: &[(&str, u64)] = &[
        ("1_byte_val_42", 42),
        ("2_byte_val_300", 300),
        ("3_byte_val_100000", 100_000),
        ("5_byte_val_4billion", 4_000_000_000),
        ("8_byte_val_2pow55", 1u64 << 55),
        ("9_byte_val_max63bit", u64::MAX >> 1),
        ("10_byte_val_u64max", u64::MAX),
        ("zero", 0),
    ];

    for (name, val) in inputs {
        let encoded = encode(*val);
        group.bench_function(*name, |b| {
            b.iter(|| {
                let mut cursor = Cursor::new(std::hint::black_box(encoded.as_slice()));
                cursor.read_varuint().unwrap()
            })
        });
    }

    group.finish();
}

fn bench_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("roundtrip");

    let inputs: &[(&str, u64)] = &[
        ("1_byte", 42),
        ("5_byte", 4_000_000_000),
        ("9_byte", u64::MAX >> 1),
    ];

    for (name, val) in inputs {
        let val = *val;
        group.bench_function(*name, |b| {
            b.iter(|| {
                let mut buf = Vec::with_capacity(10);
                buf.write_varuint(std::hint::black_box(val)).unwrap();
                let mut cursor = Cursor::new(buf.as_slice());
                cursor.read_varuint().unwrap()
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_encode, bench_decode, bench_roundtrip);
criterion_main!(benches);
