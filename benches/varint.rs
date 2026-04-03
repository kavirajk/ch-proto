use ch_proto::{decode, encode};
use criterion::{Criterion, criterion_group, criterion_main};

fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode");

    // 1 byte (7 data bits)
    group.bench_function("1_byte_val_42", |b| {
        b.iter(|| encode(std::hint::black_box(42)))
    });

    // 2 bytes (14 data bits)
    group.bench_function("2_byte_val_300", |b| {
        b.iter(|| encode(std::hint::black_box(300)))
    });

    // 3 bytes (21 data bits)
    group.bench_function("3_byte_val_100000", |b| {
        b.iter(|| encode(std::hint::black_box(100_000)))
    });

    // 5 bytes (35 data bits) — typical row count / block size range
    group.bench_function("5_byte_val_4billion", |b| {
        b.iter(|| encode(std::hint::black_box(4_000_000_000u64)))
    });

    // 8 bytes (56 data bits)
    group.bench_function("8_byte_val_2pow55", |b| {
        b.iter(|| encode(std::hint::black_box(1u64 << 55)))
    });

    // 9 bytes (63 data bits) — max varuint per CH spec
    group.bench_function("9_byte_val_max63bit", |b| {
        b.iter(|| encode(std::hint::black_box(u64::MAX >> 1)))
    });

    // 10 bytes — full u64::MAX
    group.bench_function("10_byte_val_u64max", |b| {
        b.iter(|| encode(std::hint::black_box(u64::MAX)))
    });

    // Zero — empty output edge case
    group.bench_function("zero", |b| {
        b.iter(|| encode(std::hint::black_box(0)))
    });

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
            b.iter(|| decode(std::hint::black_box(&encoded)))
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
            b.iter(|| decode(&encode(std::hint::black_box(val))))
        });
    }

    group.finish();
}

criterion_group!(benches, bench_encode, bench_decode, bench_roundtrip);
criterion_main!(benches);
