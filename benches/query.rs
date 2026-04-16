use ch_proto::proto::{
    client_info::{ClientInfo, QueryKind},
    query::{Param, Query, Setting, Stage},
    wire::{ProtoRead, ProtoWrite},
};
use criterion::{criterion_group, criterion_main, Criterion};
use std::io::Cursor;

const PROTOCOL: u32 = 54460;

fn make_client_info() -> ClientInfo {
    ClientInfo {
        query_kind: QueryKind::InitialQuery,
        initial_user: "default".to_string(),
        initial_query_id: "q-1".to_string(),
        initial_address: "127.0.0.1:0".to_string(),
        initial_time: Some(0),
        query_interface: 1,
        os_user: "user".to_string(),
        client_hostname: "host".to_string(),
        client_name: "client".to_string(),
        version_major: 1,
        version_minor: 0,
        protocol_version: PROTOCOL as u64,
        quota_key: Some("".to_string()),
        distributed_depth: Some(0),
        version_patch: Some(0),
        collaborate_with_initiator: Some(false),
        obsolete_count_participating_replicas: Some(0),
        count_current_replicas: Some(0),
    }
}

fn make_query_simple() -> Query {
    Query {
        query_id: "test-query-1".to_string(),
        client_info: make_client_info(),
        settings: vec![],
        cluster_secret: "".to_string(),
        stage: Stage::Complete,
        compression: false,
        body: "SELECT 1".to_string(),
        params: vec![],
        protocol_version: PROTOCOL as u64,
    }
}

fn make_query_complex() -> Query {
    Query {
        query_id: "complex-query".to_string(),
        client_info: make_client_info(),
        settings: vec![
            Setting { key: "max_threads".to_string(), value: "4".to_string(), important: true, custom: false, obsolete: false },
            Setting { key: "max_memory_usage".to_string(), value: "10000000".to_string(), important: false, custom: false, obsolete: false },
            Setting { key: "max_execution_time".to_string(), value: "60".to_string(), important: false, custom: false, obsolete: false },
        ],
        cluster_secret: "".to_string(),
        stage: Stage::Complete,
        compression: false,
        body: "SELECT * FROM system.numbers LIMIT 1000".to_string(),
        params: vec![
            Param { key: "limit".to_string(), value: "1000".to_string() },
            Param { key: "offset".to_string(), value: "0".to_string() },
        ],
        protocol_version: PROTOCOL as u64,
    }
}

fn bench_client_info(c: &mut Criterion) {
    let mut group = c.benchmark_group("client_info");
    let ci = make_client_info();

    group.bench_function("encode", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(256);
            ci.encode(std::hint::black_box(&mut buf), PROTOCOL).unwrap();
            buf
        })
    });

    let mut encoded = Vec::new();
    ci.encode(&mut encoded, PROTOCOL).unwrap();
    group.bench_function("decode", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(std::hint::black_box(encoded.as_slice()));
            ClientInfo::decode(&mut cursor, PROTOCOL).unwrap()
        })
    });

    group.bench_function("roundtrip", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(256);
            ci.encode(std::hint::black_box(&mut buf), PROTOCOL).unwrap();
            let mut cursor = Cursor::new(buf.as_slice());
            ClientInfo::decode(&mut cursor, PROTOCOL).unwrap()
        })
    });

    group.finish();
}

fn bench_setting(c: &mut Criterion) {
    let mut group = c.benchmark_group("setting");

    let s = Setting {
        key: "max_threads".to_string(),
        value: "4".to_string(),
        important: true,
        custom: false,
        obsolete: false,
    };

    group.bench_function("encode", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(64);
            s.encode(std::hint::black_box(&mut buf)).unwrap();
            buf
        })
    });

    let mut encoded = Vec::new();
    s.encode(&mut encoded).unwrap();
    group.bench_function("decode", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(std::hint::black_box(encoded.as_slice()));
            Setting::decode(&mut cursor).unwrap()
        })
    });

    group.finish();
}

fn bench_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("query");

    // Simple query (no settings, no params)
    let q_simple = make_query_simple();
    group.bench_function("encode_simple", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(512);
            q_simple.encode(std::hint::black_box(&mut buf)).unwrap();
            buf
        })
    });

    let mut encoded_simple = Vec::new();
    q_simple.encode(&mut encoded_simple).unwrap();
    group.bench_function("decode_simple", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(std::hint::black_box(&encoded_simple[1..]));
            Query::decode(&mut cursor, PROTOCOL).unwrap()
        })
    });

    // Complex query (settings + params)
    let q_complex = make_query_complex();
    group.bench_function("encode_complex", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(1024);
            q_complex.encode(std::hint::black_box(&mut buf)).unwrap();
            buf
        })
    });

    let mut encoded_complex = Vec::new();
    q_complex.encode(&mut encoded_complex).unwrap();
    group.bench_function("decode_complex", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(std::hint::black_box(&encoded_complex[1..]));
            Query::decode(&mut cursor, PROTOCOL).unwrap()
        })
    });

    group.bench_function("roundtrip_simple", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(512);
            q_simple.encode(std::hint::black_box(&mut buf)).unwrap();
            let mut cursor = Cursor::new(&buf[1..]);
            Query::decode(&mut cursor, PROTOCOL).unwrap()
        })
    });

    group.bench_function("roundtrip_complex", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(1024);
            q_complex.encode(std::hint::black_box(&mut buf)).unwrap();
            let mut cursor = Cursor::new(&buf[1..]);
            Query::decode(&mut cursor, PROTOCOL).unwrap()
        })
    });

    group.finish();
}

criterion_group!(benches, bench_client_info, bench_setting, bench_query);
criterion_main!(benches);
