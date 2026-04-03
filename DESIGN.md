# ClickHouse Native TCP Protocol in Rust — Implementation Plan

## Context

**Goal:** Implement ClickHouse's native TCP protocol in Rust as a deep learning exercise, structured as incremental problems to solve. By the end, have enough understanding to write a protocol spec.

**Two stages:**
- **Stage A:** Protocol version 54460 (matching ch-go)
- **Stage B:** Protocol version 54483 (current ClickHouse latest)

**Side question answer:** No — the native TCP protocol **only** uses the Native data format for block serialization. Other formats (JSON, CSV, TSV, etc.) are HTTP-interface-only. Over TCP, data is always column-oriented binary blocks. The "Native" in "native protocol" refers to ClickHouse's own binary TCP wire protocol (vs HTTP, MySQL, PostgreSQL wire protocols).

**Approach:** Yes, implement core data types first, then add remaining incrementally. This is the right call — you need UInt/Int/String/Float to decode *any* server response (Progress, ProfileEvents, Logs are all data blocks with typed columns).

**Reference codebases:**
- `~/src/ch-go/main/proto/` — primary reference (Go client, protocol v54460)
- `~/src/ClickHouse/src/` — authoritative source (C++ server, protocol v54483)
- `~/src/chwire/` — your packet inspector for debugging

**Testing:** `docker run -d -p 9000:9000 clickhouse/clickhouse-server:latest` + chwire for wire-level verification.

---

## Stage A: Match ch-go (Protocol Version 54460)

### Phase 1: Wire Primitives (Problems 1-3)

#### Problem 1: VarUInt (LEB128)
Implement `read_varuint` / `write_varuint` — unsigned LEB128, max 9 bytes (63 bits). This is the foundation of everything.

- Test: roundtrip 0, 1, 127, 128, 16384, u64::MAX
- Golden-test against ch-go output
- **Ref:** `ch-go/main/proto/buffer.go:73-77`, `ClickHouse/src/IO/VarInt.h`

#### Problem 2: Strings, Fixed Integers, Bool
- Strings: VarUInt length + raw bytes (NOT null-terminated)
- Fixed ints: little-endian u8/u16/u32/u64/i32/i64
- Bool: UInt8 (0/1)
- Build `Buffer` (growable writer) and `Reader` (wrapping `impl Read`)
- **Ref:** `ch-go/main/proto/buffer.go:95-163`, `ch-go/main/proto/reader.go`

#### Problem 3: Packet Enums and Feature Flags
- `ClientPacket` enum (0-13), `ServerPacket` enum (0-18)
- `Feature` constants with `is_supported(version)` method
- 23 features for Stage A (BlockInfo=51903 through ServerQueryTimeInProgress=54460)
- **Ref:** `ClickHouse/src/Core/Protocol.h`, `ClickHouse/src/Core/ProtocolDefines.h`, `ch-go/main/proto/feature.go`

---

### Phase 2: Handshake (Problems 4-6)

#### Problem 4: Client Hello
Send ClientHello to real ClickHouse, read raw bytes back.

Wire format: `VarUInt(0)` + name(String) + major(VarUInt) + minor(VarUInt) + protocol_version(VarUInt) + database(String) + user(String) + password(String)

- Claim protocol_version=54460
- **Ref:** `ch-go/main/proto/client_hello.go:24-33`

#### Problem 5: Server Hello
Decode server response. Version-aware fields:
- Always: name, major, minor, revision
- `>=54058`: timezone (String)
- `>=54372`: display_name (String)
- `>=54401`: patch (VarUInt)

Negotiate version: `min(client_claimed, server_revision)`. Handle Exception packet (wrong password).

- **Ref:** `ch-go/main/proto/server_hello.go:58-103`

#### Problem 6: Addendum + Connection Struct
If negotiated version `>=54458`: send quota_key (empty String).

Wrap into `Connection::connect(addr, user, password, database) -> Result<Connection>` doing: TCP connect → ClientHello → ServerHello → version negotiation → Addendum.

- **Ref:** `ch-go/main/handshake.go:85-191`

---

### Phase 3: Ping and Exceptions (Problems 7-8)

#### Problem 7: Ping/Pong
Send VarUInt(4), expect VarUInt(4) back. Simplest request-response cycle.

#### Problem 8: Exception Decoding
`Exception`: code(Int32 LE fixed) + name(String) + message(String) + stack_trace(String) + has_nested(Bool). Recursive if nested.

Wire into packet loop: any ServerPacket::Exception → decode → return error.

- **Ref:** `ch-go/main/proto/exception.go`

---

### Phase 4: Query & Data Blocks (Problems 9-15)

#### Problem 9: Send a Query (`SELECT 1`)
The big one. Encode the Query packet with all version-gated fields:

1. `VarUInt(1)` — ClientPacket::Query
2. query_id (String)
3. ClientInfo (many version-gated fields — see `ch-go/main/proto/client_info.go:65-125`)
4. Settings (key/value pairs with flags byte, terminated by empty string key)
5. Inter-server secret (empty string if `>=54441`)
6. Stage (VarUInt, 2=Complete)
7. Compression (VarUInt, 0=disabled)
8. Query body (String)
9. Parameters (if `>=54459`, terminated by empty string key)

**Critical:** After Query, send empty Data block (ClientPacket::Data + empty table name + empty block) to signal "no external tables". Without this, server hangs.

- **Ref:** `ch-go/main/proto/query.go:184-213`, `ch-go/main/proto/client_info.go`

#### Problem 10: Block Info and Empty Blocks
BlockInfo uses field-id/value pairs terminated by id=0:
- `VarUInt(1)` + Bool(overflows)
- `VarUInt(2)` + Int32(bucket_num, default -1, LE fixed)
- `VarUInt(0)` — end

Block header: `num_columns(VarUInt)` + `num_rows(VarUInt)`. Empty block = 0 columns, 0 rows.

- **Ref:** `ch-go/main/proto/block.go:12-65`

#### Problem 11: Core Column Types — Fixed-Width
Decode UInt8/16/32/64, Int8/16/32/64, Float32/64, Bool.

Fixed-width columns: `rows * sizeof(type)` bytes, contiguous, little-endian. No length prefix per value.

Per-column wire: name(String) + type(String) + custom_serialization(Bool if `>=54454`, must be false) + raw data.

- Test: `SELECT number FROM system.numbers LIMIT 100` (UInt64)
- **Ref:** `ch-go/main/proto/col_uint8_gen.go`, `ch-go/main/proto/block.go:248-305`

#### Problem 12: String and FixedString
- String column: per-row VarUInt(length) + bytes
- FixedString(N): per-row exactly N bytes, no length prefix. Parse N from type string.
- Test: `SELECT name FROM system.databases`
- **Ref:** `ch-go/main/proto/col_str.go:133-167`

#### Problem 13: Progress, ProfileInfo, ProfileEvents
- Progress: rows, bytes, total_rows (VarUInt), + wrote_rows/wrote_bytes if `>=54420`, + elapsed_ns if `>=54460`
- ProfileInfo: rows, blocks, bytes (VarUInt), applied_limit (Bool), rows_before_limit (VarUInt), calculated_rows_before_limit (Bool)
- ProfileEvents/Log: decoded as Data blocks with specific column schemas
- **Ref:** `ch-go/main/proto/progress.go`, `ch-go/main/proto/profile.go`

#### Problem 14: Date, DateTime, UUID
- Date: UInt16 (days since epoch)
- Date32: Int32 (days since epoch)
- DateTime: UInt32 (unix seconds). Type string may have timezone: `DateTime('UTC')`
- DateTime64(P): Int64. Precision P from type string.
- UUID: 16 bytes (two UInt64s)
- **Ref:** `ch-go/main/proto/col_date_gen.go`, `col_datetime.go`, `col_uuid.go`

#### Problem 15: Complete SELECT Loop
Wire everything into `Connection::query(sql) -> Result<QueryResult>`:

```
loop {
    match server_packet {
        Data => decode block (first is empty header, rest are results)
        Progress => accumulate
        ProfileInfo => store
        ProfileEvents => decode as data block
        Log => decode as data block
        TableColumns => store
        Totals => decode block
        Extremes => decode block
        EndOfStream => break
        Exception => return error
    }
}
```

- Test: multi-column, empty results, large results (multiple Data blocks), exceptions
- **Ref:** `ch-go/main/query.go:100+`

---

### Phase 5: INSERT, Compression, Composite Types (Problems 16-20)

#### Problem 16: INSERT
Flow: send Query(`INSERT INTO t VALUES`) → read server's schema Data block (column types, 0 rows) → send Data blocks with actual rows → send empty Data block → read EndOfStream.

- **Ref:** `ch-go/main/proto/block.go:152-184` (EncodeRawBlock)

#### Problem 17: LZ4 Compression
Compression frame wraps block content (NOT packet type or table name):
- 16 bytes: CityHash128 checksum
- 1 byte: method (0x82=LZ4, 0x90=ZSTD)
- 4 bytes LE: compressed size (includes 9-byte header)
- 4 bytes LE: uncompressed size
- N bytes: compressed data

Enable via compression=1 in Query packet. Use `lz4` and `cityhash` crates.

- **Ref:** `ch-go/main/compress/` (reader.go, writer.go, compress.go)

#### Problem 18: Nullable, Array, Tuple
- Nullable(T): UInt8 null mask (1=null) for all rows, then T data for all rows
- Array(T): UInt64 cumulative offsets, then T data for total elements
- Tuple(T1,T2,...): each element encoded as separate column sequentially
- Build a type string parser for nested types
- **Ref:** `ch-go/main/proto/col_nullable.go`, `col_arr.go`, `col_tuple.go`

#### Problem 19: LowCardinality
Dictionary encoding. Has state written once per column per query:
1. Int64 key_serialization_version (must be 1)
2. Per-block: Int64 metadata (flags + key type), Int64 dict_size, dict data, Int64 keys_count, keys data

- **Ref:** `ch-go/main/proto/col_low_cardinality.go`

#### Problem 20: Remaining Types
- Int128/UInt128 (16 bytes LE), Int256/UInt256 (32 bytes LE)
- Decimal32/64/128/256 (Int storage with scale from type string)
- Enum8/16 (Int8/Int16 storage, mapping from type string)
- IPv4 (UInt32), IPv6 (FixedString(16))
- Map(K,V) — wire-identical to Array(Tuple(K,V))
- Nothing (zero-size)

---

## Stage B: Catch Up to 54483 (Problems 21-24)

#### Problem 21: Versions 54461-54465
- **54461:** Server Hello adds password complexity rules (VarUInt count + count*(String,String))
- **54462:** Server Hello adds UInt64 nonce for interserver auth
- **54463:** Progress adds total_bytes_to_read (VarUInt)
- **54464:** New ServerPacket::TimezoneUpdate(17) — single String
- **54465:** Sparse serialization — custom_serialization flag can now be true

#### Problem 22: Versions 54466-54474
- **54466:** SSH auth (challenge-response flow with new packet types 11/12/18)
- **54469:** ProfileInfo adds rows_before_aggregation fields
- **54470:** Chunked protocol — Addendum gets proto_send_chunked/proto_recv_chunked strings; packet framing with 4-byte size headers
- **54471:** Addendum gets parallel_replicas_protocol_version (VarUInt); Server Hello too
- **54474:** Server Hello includes server settings (same format as query settings)

**Verified ServerHello field ordering (from TCPHandler.cpp:2205-2264):**
```
 1. VarUInt(0)                    — packet type (Server::Hello = 0)
 2. String(VERSION_NAME)          — always
 3. VarUInt(VERSION_MAJOR)        — always
 4. VarUInt(VERSION_MINOR)        — always
 5. VarUInt(DBMS_TCP_PROTOCOL_VERSION) — always (server revision)
 6. VarUInt(PARALLEL_REPLICAS_VERSION) — if client >= 54471
 7. String(timezone)              — if client >= 54058
 8. String(display_name)          — if client >= 54372
 9. VarUInt(VERSION_PATCH)        — if client >= 54401
10. String(proto_caps.send)       — if client >= 54470
11. String(proto_caps.recv)       — if client >= 54470
12. VarUInt(rules_count) + count*(String pattern, String message) — if client >= 54461
13. Int64 LE(nonce)               — if client >= 54462 (8 bytes fixed, NOT VarUInt)
14. Settings(server_settings)     — if client >= 54474 (same format as query settings, terminated by empty string)
15. VarUInt(QUERY_PLAN_SERIALIZATION_VERSION) — if client >= 54477
16. VarUInt(CLUSTER_PROCESSING_PROTOCOL_VERSION) — if client >= 54479
```

**Verified Addendum ordering (from Connection.cpp:504-519):**
```
1. String(quota_key)              — if version >= 54458
2. String(proto_send_chunked)     — if version >= 54470
3. String(proto_recv_chunked)     — if version >= 54470
4. VarUInt(parallel_replicas_version) — if version >= 54471
```

#### Problem 23: Versions 54475-54478
- **54475:** JSON column type support
- **54476:** JWT auth in interserver communication
- **54477:** Server Hello adds query_plan_serialization_version; ClientPacket::QueryPlan(13)
- **54478:** Binary type encoding (compact binary instead of type name strings)

#### Problem 24: Versions 54479-54483 + Final Integration
- **54479:** cluster_function_protocol_version in ServerHello (field 16)
- **54480:** Out-of-order buckets in aggregation (server-internal, no client wire changes)
- **54481:** Server Log and ProfileEvents blocks may use compression
- **54482:** Replicated serialization (server-internal)
- **54483:** Nullable sparse serialization
- Polish: clean public API, error types, comprehensive integration tests
- Benchmark against ch-go

---

## Verification Strategy

For every problem:
1. **Unit tests:** encode/decode roundtrips with known byte sequences
2. **Integration tests:** connect to real ClickHouse via docker
3. **Wire verification:** use chwire to inspect packets when debugging
4. **Golden tests:** compare byte output against ch-go for critical encodings

---

## Key Files Quick Reference

| What | ClickHouse Server | ch-go |
|------|------------------|-------|
| Protocol versions | `src/Core/ProtocolDefines.h` | `proto/feature.go` |
| Packet types | `src/Core/Protocol.h` | `proto/client_code.go`, `proto/server_code.go` |
| TCP handler | `src/Server/TCPHandler.cpp` | `handshake.go`, `query.go` |
| Block format | `src/Formats/NativeReader.cpp` | `proto/block.go` |
| Compression | `src/Compression/CompressionInfo.h` | `compress/` |
| Query packet | `src/Client/Connection.cpp:823-1036` | `proto/query.go`, `proto/client_info.go` |
| Progress | `src/IO/Progress.cpp` | `proto/progress.go` |
