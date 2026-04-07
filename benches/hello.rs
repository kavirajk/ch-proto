use ch_proto::{
    hello::{ClientHello, ServerHello},
    proto::{ProtoRead, ProtoWrite},
};
use criterion::{Criterion, criterion_group, criterion_main};
use std::io::Cursor;

fn make_client_hello() -> ClientHello {
    ClientHello {
        name: "clickhouse-rs".to_string(),
        version_major: 21,
        version_minor: 8,
        protocol_version: 54460,
        database: "default".to_string(),
        user: "admin".to_string(),
        password: "secret".to_string(),
    }
}

fn make_server_hello() -> ServerHello {
    ServerHello {
        name: "ClickHouse".to_string(),
        version_major: 21,
        version_minor: 8,
        protocol_version: 54460,
        timezone: Some("UTC".to_string()),
        display_name: Some("production-1".to_string()),
        version_patch: Some(3),
    }
}

fn bench_client_hello(c: &mut Criterion) {
    let mut group = c.benchmark_group("client_hello");
    let hello = make_client_hello();

    group.bench_function("encode", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(128);
            hello.encode(std::hint::black_box(&mut buf)).unwrap();
            buf
        })
    });

    let mut encoded = Vec::new();
    hello.encode(&mut encoded).unwrap();
    group.bench_function("decode", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(std::hint::black_box(&encoded[1..]));
            ClientHello::decode(&mut cursor).unwrap()
        })
    });

    group.bench_function("roundtrip", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(128);
            hello.encode(std::hint::black_box(&mut buf)).unwrap();
            let mut cursor = Cursor::new(&buf[1..]);
            ClientHello::decode(&mut cursor).unwrap()
        })
    });

    group.finish();
}

fn bench_server_hello(c: &mut Criterion) {
    let mut group = c.benchmark_group("server_hello");
    let hello = make_server_hello();

    let protocols: &[(&str, u32)] = &[
        ("no_features_v50000", 50000),
        ("timezone_only_v54058", 54058),
        ("all_features_v54460", 54460),
    ];

    for (name, protocol) in protocols {
        let protocol = *protocol;
        let hello_for_proto = ServerHello {
            name: hello.name.clone(),
            version_major: hello.version_major,
            version_minor: hello.version_minor,
            protocol_version: hello.protocol_version,
            timezone: if protocol >= 54058 { Some("UTC".to_string()) } else { None },
            display_name: if protocol >= 54372 { Some("production-1".to_string()) } else { None },
            version_patch: if protocol >= 54401 { Some(3) } else { None },
        };

        group.bench_function(format!("encode_{name}"), |b| {
            b.iter(|| {
                let mut buf = Vec::with_capacity(128);
                hello_for_proto.encode(std::hint::black_box(&mut buf), protocol).unwrap();
                buf
            })
        });

        let mut encoded = Vec::new();
        hello_for_proto.encode(&mut encoded, protocol).unwrap();
        group.bench_function(format!("decode_{name}"), |b| {
            b.iter(|| {
                let mut cursor = Cursor::new(std::hint::black_box(&encoded[1..]));
                ServerHello::decode(&mut cursor, protocol).unwrap()
            })
        });
    }

    group.bench_function("roundtrip_all_features", |b| {
        let protocol = 54460u32;
        b.iter(|| {
            let mut buf = Vec::with_capacity(128);
            hello.encode(std::hint::black_box(&mut buf), protocol).unwrap();
            let mut cursor = Cursor::new(&buf[1..]);
            ServerHello::decode(&mut cursor, protocol).unwrap()
        })
    });

    group.finish();
}

criterion_group!(benches, bench_client_hello, bench_server_hello);
criterion_main!(benches);
