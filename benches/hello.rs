use ch_proto::{
    exception::ServerException,
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

fn make_exception() -> ServerException {
    ServerException {
        code: 13,
        name: "DB::Exception".to_string(),
        message: "Unexpected packet from client (no user in Hello package)".to_string(),
        stack_trace: "0. DB::Exception::Exception()\n1. DB::TCPHandler::runImpl()".to_string(),
        nested: false,
    }
}

fn make_exception_short() -> ServerException {
    ServerException {
        code: 0,
        name: "E".to_string(),
        message: "err".to_string(),
        stack_trace: "".to_string(),
        nested: false,
    }
}

fn bench_exception(c: &mut Criterion) {
    let mut group = c.benchmark_group("exception");
    let mut exc = make_exception();

    group.bench_function("encode", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(256);
            exc.encode(std::hint::black_box(&mut buf)).unwrap();
            buf
        })
    });

    let mut encoded = Vec::new();
    exc.encode(&mut encoded).unwrap();
    group.bench_function("decode", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(std::hint::black_box(&encoded[1..]));
            ServerException::decode(&mut cursor).unwrap()
        })
    });

    group.bench_function("roundtrip", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(256);
            exc.encode(std::hint::black_box(&mut buf)).unwrap();
            let mut cursor = Cursor::new(&buf[1..]);
            ServerException::decode(&mut cursor).unwrap()
        })
    });

    let mut short_exc = make_exception_short();
    let mut short_encoded = Vec::new();
    short_exc.encode(&mut short_encoded).unwrap();
    group.bench_function("decode_short", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(std::hint::black_box(&short_encoded[1..]));
            ServerException::decode(&mut cursor).unwrap()
        })
    });

    group.finish();
}

fn bench_packet_dispatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("packet_dispatch");

    // ServerHello packet
    let hello = make_server_hello();
    let protocol = 54460u32;
    let mut hello_buf = Vec::new();
    hello.encode(&mut hello_buf, protocol).unwrap();

    group.bench_function("dispatch_hello", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(std::hint::black_box(hello_buf.as_slice()));
            let pkt_type = cursor.read_varuint().unwrap() as u8;
            match pkt_type {
                0 => ServerHello::decode(&mut cursor, protocol).unwrap(),
                _ => panic!(),
            }
        })
    });

    // Exception packet
    let mut exc = make_exception();
    let mut exc_buf = Vec::new();
    exc.encode(&mut exc_buf).unwrap();

    group.bench_function("dispatch_exception", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(std::hint::black_box(exc_buf.as_slice()));
            let pkt_type = cursor.read_varuint().unwrap() as u8;
            match pkt_type {
                2 => ServerException::decode(&mut cursor).unwrap(),
                _ => panic!(),
            }
        })
    });

    // Pong packet
    let mut pong_buf = Vec::new();
    pong_buf.write_varuint(4).unwrap(); // ServerPacket::Pong
    group.bench_function("dispatch_pong", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(std::hint::black_box(pong_buf.as_slice()));
            let pkt_type = cursor.read_varuint().unwrap() as u8;
            assert_eq!(pkt_type, 4);
        })
    });

    group.finish();
}

fn bench_ping(c: &mut Criterion) {
    let mut group = c.benchmark_group("ping");

    // Encode ping packet
    group.bench_function("encode", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(1);
            buf.write_varuint(std::hint::black_box(4u64)).unwrap();
            buf
        })
    });

    // Decode pong packet
    let mut pong_buf = Vec::new();
    pong_buf.write_varuint(4).unwrap();
    group.bench_function("decode_pong", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(std::hint::black_box(pong_buf.as_slice()));
            cursor.read_varuint().unwrap()
        })
    });

    // Full ping/pong roundtrip (encode ping + decode pong, no network)
    group.bench_function("roundtrip_wire", |b| {
        b.iter(|| {
            let mut ping_buf = Vec::with_capacity(1);
            ping_buf.write_varuint(std::hint::black_box(4u64)).unwrap();

            let mut pong_buf = Vec::with_capacity(1);
            pong_buf.write_varuint(4u64).unwrap();
            let mut cursor = Cursor::new(pong_buf.as_slice());
            cursor.read_varuint().unwrap()
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_client_hello,
    bench_server_hello,
    bench_exception,
    bench_packet_dispatch,
    bench_ping,
);
criterion_main!(benches);
