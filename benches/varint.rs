use ch_proto::proto::wire::{ProtoRead, ProtoWrite};
use criterion::{criterion_group, criterion_main, Criterion};
use std::io::Cursor;

fn bench_write_varuint(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_varuint");

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

fn bench_read_varuint(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_varuint");

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
        let mut encoded = Vec::new();
        encoded.write_varuint(*val).unwrap();
        group.bench_function(*name, |b| {
            b.iter(|| {
                let mut cursor = Cursor::new(std::hint::black_box(encoded.as_slice()));
                cursor.read_varuint().unwrap()
            })
        });
    }

    group.finish();
}

fn bench_write_fixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_fixed");

    group.bench_function("u8", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(8);
            buf.write_u8(std::hint::black_box(0xFF)).unwrap();
        })
    });
    group.bench_function("u16", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(8);
            buf.write_u16(std::hint::black_box(0x1234)).unwrap();
        })
    });
    group.bench_function("u32", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(8);
            buf.write_u32(std::hint::black_box(0xDEADBEEF)).unwrap();
        })
    });
    group.bench_function("u64", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(8);
            buf.write_u64(std::hint::black_box(u64::MAX)).unwrap();
        })
    });
    group.bench_function("i32", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(8);
            buf.write_i32(std::hint::black_box(-1)).unwrap();
        })
    });
    group.bench_function("i64", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(8);
            buf.write_i64(std::hint::black_box(i64::MIN)).unwrap();
        })
    });
    group.bench_function("bool", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(8);
            buf.write_bool(std::hint::black_box(true)).unwrap();
        })
    });

    group.finish();
}

fn bench_read_fixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_fixed");

    let mut u8_buf = Vec::new();
    u8_buf.write_u8(0xFF).unwrap();
    group.bench_function("u8", |b| {
        b.iter(|| {
            Cursor::new(std::hint::black_box(u8_buf.as_slice()))
                .read_u8()
                .unwrap()
        })
    });

    let mut u16_buf = Vec::new();
    u16_buf.write_u16(0x1234).unwrap();
    group.bench_function("u16", |b| {
        b.iter(|| {
            Cursor::new(std::hint::black_box(u16_buf.as_slice()))
                .read_u16()
                .unwrap()
        })
    });

    let mut u32_buf = Vec::new();
    u32_buf.write_u32(0xDEADBEEF).unwrap();
    group.bench_function("u32", |b| {
        b.iter(|| {
            Cursor::new(std::hint::black_box(u32_buf.as_slice()))
                .read_u32()
                .unwrap()
        })
    });

    let mut u64_buf = Vec::new();
    u64_buf.write_u64(u64::MAX).unwrap();
    group.bench_function("u64", |b| {
        b.iter(|| {
            Cursor::new(std::hint::black_box(u64_buf.as_slice()))
                .read_u64()
                .unwrap()
        })
    });

    let mut i32_buf = Vec::new();
    i32_buf.write_i32(-1).unwrap();
    group.bench_function("i32", |b| {
        b.iter(|| {
            Cursor::new(std::hint::black_box(i32_buf.as_slice()))
                .read_i32()
                .unwrap()
        })
    });

    let mut i64_buf = Vec::new();
    i64_buf.write_i64(i64::MIN).unwrap();
    group.bench_function("i64", |b| {
        b.iter(|| {
            Cursor::new(std::hint::black_box(i64_buf.as_slice()))
                .read_i64()
                .unwrap()
        })
    });

    let mut bool_buf = Vec::new();
    bool_buf.write_bool(true).unwrap();
    group.bench_function("bool", |b| {
        b.iter(|| {
            Cursor::new(std::hint::black_box(bool_buf.as_slice()))
                .read_bool()
                .unwrap()
        })
    });

    group.finish();
}

fn bench_string(c: &mut Criterion) {
    let mut group = c.benchmark_group("string");

    let cases: &[(&str, &str)] = &[
        ("empty", ""),
        ("short_5b", "hello"),
        ("medium_100b", &"x".repeat(100)),
        ("long_10kb", &"y".repeat(10_000)),
    ];

    for (name, s) in cases {
        group.bench_function(format!("write_{name}"), |b| {
            b.iter(|| {
                let mut buf = Vec::with_capacity(s.len() + 10);
                buf.write_string(std::hint::black_box(s)).unwrap();
                buf
            })
        });
    }

    for (name, s) in cases {
        let mut encoded = Vec::new();
        encoded.write_string(s).unwrap();
        group.bench_function(format!("read_{name}"), |b| {
            b.iter(|| {
                let mut cursor = Cursor::new(std::hint::black_box(encoded.as_slice()));
                cursor.read_string().unwrap()
            })
        });
    }

    group.finish();
}

fn bench_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("roundtrip");

    let varuint_inputs: &[(&str, u64)] = &[
        ("varuint_1byte", 42),
        ("varuint_5byte", 4_000_000_000),
        ("varuint_9byte", u64::MAX >> 1),
    ];

    for (name, val) in varuint_inputs {
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

    group.bench_function("u64", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(8);
            buf.write_u64(std::hint::black_box(u64::MAX)).unwrap();
            let mut cursor = Cursor::new(buf.as_slice());
            cursor.read_u64().unwrap()
        })
    });

    group.bench_function("string_short", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(16);
            buf.write_string(std::hint::black_box("hello")).unwrap();
            let mut cursor = Cursor::new(buf.as_slice());
            cursor.read_string().unwrap()
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_write_varuint,
    bench_read_varuint,
    bench_write_fixed,
    bench_read_fixed,
    bench_string,
    bench_roundtrip,
);
criterion_main!(benches);
