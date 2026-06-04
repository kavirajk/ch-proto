# ClickHouse Native TCP Protocol — Implementation & Spec Plan

## Context

**Goals (two deliverables):**

1. **Rust client library** — a working-enough client to demonstrate end-to-end protocol understanding, validate the spec against a real server, and serve as a reference implementation.
2. **Protocol specification (SPEC.md)** — an authoritative, self-contained, language-agnostic spec for the ClickHouse native TCP protocol and Native data format.

**Source-of-truth references:**
- `~/src/ClickHouse/src/` — C++ server, authoritative source at current protocol version `54483`. Do not cite in the spec (spec is self-contained); use only as a verification source during implementation.
- `~/src/ch-go/main/proto/` — minimalist Go reference, covers up to protocol `54460`.
- `~/src/clickhouse-go/main/` — production Go driver, covers up to `~54483` including full JSON/Dynamic/Variant implementations.

**Current server target:** `DBMS_TCP_PROTOCOL_VERSION = 54483`. Defined at `ClickHouse/src/Core/ProtocolDefines.h:139`.

**Testing infrastructure:** `make up` starts a ClickHouse container via docker-compose. `make test-unit` for Rust unit tests. `make test-integration` runs the full integration suite against the running server.

---

## Status Summary

Status legend: ✅ complete · ⚠️ partial · ⏳ pending · ❌ deferred

| Phase | Topic | Problems | Status |
|-------|-------|----------|--------|
| 1     | Wire primitives & I/O scaffold              | 1–5    | ✅ |
| 2     | Handshake                                    | 6–9    | ✅ |
| 3     | Ping                                         | 10     | ✅ |
| 4     | Query phase scaffold                         | 11–19  | ✅ |
| 5     | Basic data types                             | 20–24  | ✅ |
| 6     | Composite types (Nullable / Array / Tuple / Map / Nested) | 25–29 | ✅ |
| 7     | More fixed-width and parameterized types     | 30–36  | ✅ |
| 8     | Versioned / stateful types                   | 37–41  | ⚠️ (LowCardinality + JSON Tier 1 done; Variant / Dynamic / JSON Tier 2 deferred) |
| 9     | Compression                                  | 42–43  | ⚠️ (frame primitives done; connection-level integration pending) |
| 10    | INSERT path                                  | 44–45  | ✅ |
| 11    | Bring spec up to server v54483               | 46–65  | ⚠️ (46 done; 47–65 pending) |
| 12    | Polish and presentation                      | 66–69  | ⏳ |
| 13    | Spec completion                              | 70–73  | ⚠️ (composite + versioned + compression sections done; chunked-protocol pending) |

**Test coverage at the time of writing:** 275 unit tests + 89 integration tests, all passing.

Differential harness against ClickHouse's `tests/queries/0_stateless` corpus, via the `ch-tsv` wrapper binary, parallel-8 execution (`make test-differential-full`):

| Run | Tests addressable | PASS | Pass rate |
|-----|------------------:|------:|----------:|
| Stage 0 (TSV primitives, allowlist) | 10 | 10 | 100% |
| Stage 1 (broader TSV, SELECT-only filter) | 1,141 | 969 | 84.9% |
| Stage 2 (CREATE/INSERT/SET unlocked via per-test DB) | 3,753 | 3,050 | 81.3% |
| Stage 3 (`-- { serverError NAME }` markers honored) | 4,909 | **3,941** | **80.3%** |

Stage 2 unlocked the ~4,200 tests with DDL/DML by wrapping each test in a `CREATE DATABASE test_<pid>_<n>; USE ...; DROP DATABASE` envelope inside the harness — the same pattern the canonical `clickhouse-test` runner uses. SET/SETTINGS pass through transparently because they're regular SQL statements on the wire.

Stage 3 unlocked the ~1,200 negative-path tests by parsing the test-hint markers (`-- { serverError NAME }`, `-- { clientError 42 }`, etc.) — same syntax as `ClickHouse/src/Client/TestHint.cpp`. Numeric and name-based codes both supported; the wrapper looks up the name→code mapping once per run via `errorCodeToName`.

**Spec deliverables:**

- `NATIVE_PROTOCOL.md` — TCP wire protocol (state machine, packets, configuration).
- `NATIVE_FORMAT.md` — Block/Column structure, data types, compression frame.
- `IMPLEMENTATION_NOTES.md` — gotchas in Symptom/Cause/Fix form + reference Rust client status.
- `SPEC.md` — short redirect note pointing to the three above.

References to spec sections in this document use the original SPEC.md numbering (e.g., "§7.11"). The mapping to the post-split layout is:

| Old SPEC.md range | New location |
|-------------------|--------------|
| §1 Overview, §2 Wire Primitives, §7.11 Block & Column, §8 Data Types, §9 Compression | `NATIVE_FORMAT.md` |
| §3 Security, §4 Versioning, §5 Packet Envelope, §6 Lifecycle, §7 (other messages), §10 Packet Type Reference, §12 Configuration | `NATIVE_PROTOCOL.md` |
| §11 Implementation Notes | `IMPLEMENTATION_NOTES.md` |

### Known problems with DESIGN.md's old assumptions (now fixed)

- **~~Stage A: v54460, Stage B: v54483~~** — The two-stage split was ambitious but the "current" target kept moving. Replaced with a single current-target plan (54483) and "tiers" of type support.
- **~~Problem 23: "54475: JSON column type support"~~** — **Wrong.** Protocol version 54475 is `QUERY_AND_LINE_NUMBERS` (script_query_number / script_line_number in ClientInfo). JSON support is gated by serialization-version prefixes inside the column data (§8.4.2 in the spec), not by a protocol-version feature.
- **~~Feature name mismatch~~** — ch-go has `FeatureJSONStrings = 54475` which is a naming error that carried into the original DESIGN.md. Do not add a feature at 54475 named after JSON.

---

## Completed Phases

### Phase 1: Wire primitives & I/O scaffold ✅

The minimum byte-level toolkit that everything else depends on.

#### Problem 1: VarUInt (LEB-128) ✅

7-bit data + 1-bit continuation, little-endian, up to 10 bytes for full UInt64 range.

**Implementation:** `read_varuint` / `write_varuint` on a `ProtoRead` / `ProtoWrite` trait over `std::io::Read` / `std::io::Write`. Bench harness in `benches/varint.rs`.

**Tests:**
- Roundtrip across single-byte (`0..=127`), two-byte (`128..=16383`), and multi-byte boundaries.
- Specific encodings against ch-go's outputs (`300` → `AC 02`, etc.).

**Spec work:** §2.1.

**References:**
- ch-go: `proto/uvariant.go`
- ClickHouse: `src/IO/VarInt.h`

---

#### Problem 2: Fixed-width integers ✅

Little-endian encode/decode for `UInt8`, `UInt16`, `UInt32`, `UInt64`, `Int8` (UInt8 cast), `Int32`, `Int64`. Wide ints (`Int128`/`UInt128`/`Int256`/`UInt256`) added later in Phase 7.

**Implementation:** `read_uN` / `write_uN` and signed variants on the wire traits. Two's complement comes for free from Rust's `to_le_bytes` on the matching type.

**Spec work:** §2.2.

---

#### Problem 3: Length-prefixed String ✅

`[VarUInt: byte_length] [byte_length bytes]`. Empty string is a single `0x00`. Bytes are not required to be valid UTF-8 on the wire; this client decodes via `String::from_utf8` and returns `InvalidData` on non-UTF-8 bytes (acceptable for protocol messages but not for general `String` columns — see Problem 23).

**Spec work:** §2.3.

---

#### Problem 4: Bool ✅

Single byte. `0x00` = false, non-zero = true (canonical `0x01`). `read_bool` / `write_bool` on the wire traits.

**Spec work:** §2.4.

---

#### Problem 5: ProtoRead / ProtoWrite traits + TCP connect ✅

Two extension traits over `std::io::Read` and `std::io::Write` providing all the per-primitive helpers above (varuint, fixed-width int, string, bool). `Connection::connect` opens a `TcpStream` to the target address. `TCP_NODELAY` is on by default for `TcpStream`. `SO_KEEPALIVE` / `TCP_KEEPIDLE` not yet applied client-side — see Problem 66.

**References:**
- ClickHouse: `src/IO/ReadHelpers.h`, `src/IO/WriteHelpers.h`, `src/IO/ReadBufferFromPocoSocket.h`, `src/Client/Connection.cpp`.

---

### Phase 2: Handshake ✅

Authentication and version negotiation over a fresh TCP connection.

#### Problem 6: ClientHello encode ✅

Fields: `client_name`, `version_major`, `version_minor`, `protocol_version`, `database`, `user`, `password`. No feature gating — all body fields are always present. Sent as the first message after TCP connect.

**Spec work:** §7.1.

---

#### Problem 7: ServerHello decode (feature-gated) ✅

Fields: `server_name`, `version_major`, `version_minor`, `protocol_version`, plus feature-gated `timezone` (TIMEZONE / v54058), `display_name` (DISPLAY_NAME / v54372), `version_patch` (VERSION_PATCH / v54401). Decoder must consult the **negotiated** protocol version, not the client's declared version.

**Spec work:** §7.2.

---

#### Problem 8: Version negotiation ✅

`negotiated_version = min(client_protocol_version, server_protocol_version)`. Computed immediately after ServerHello decode; used for every subsequent feature-gate check on the connection.

**Implementation note:** the client must declare the highest protocol version it actually supports (currently `54459` for `Feature::PARAMETERS`). Declaring lower silently disables features at encode time — see Implementation Notes §1.10.

---

#### Problem 9: Addendum (post-handshake quota_key) ✅

When `negotiated_version >= 54458` (`ADDENDUM`), the client sends a single `String quota_key` field as raw fields with no packet type prefix. External clients send empty string. Sending the addendum when the server doesn't expect it (or skipping it when it does) misaligns every subsequent message — gating must use the **negotiated** version.

**Spec work:** §7.3.

---

### Phase 3: Ping ✅

#### Problem 10: Ping/Pong ✅

Both packets carry no body — they are a single `0x04` byte on the wire (the VarUInt-encoded packet type). Stateless and uncorrelated with any query. Multiple sequential Pings on the same connection are valid.

**Implementation:** `Connection::ping` writes a Ping, flushes, and reads exactly one Pong (or Exception). The connection state oscillates `READY → READY` with no intermediate state change.

**Spec work:** §6.3, §7.4, §7.5.

---

### Phase 4: Query phase scaffold ✅

The bulk of the protocol — submitting a SQL statement and consuming the response stream.

#### Problem 11: Query packet ✅

Outer fields: `query_id`, `client_info` (Problem 12), `settings` (Problem 13), `cluster_secret` (inter-server), `stage`, `compression`, `query_body`, `parameters` (Problem 14, gated by `PARAMETERS` v54459). External clients send `cluster_secret = ""`, `stage = Complete (2)`, `compression = false` by default.

**Spec work:** §7.7.

---

#### Problem 12: ClientInfo encode ✅

Embedded inline in the Query body when `WRITE_CLIENT_INFO` (v54420) is active. 19 fields with cascading feature gates (QUOTA_KEY_IN_CLIENT_INFO, INITIAL_QUERY_START_TIME, OPEN_TELEMETRY, DISTRIBUTED_DEPTH, VERSION_PATCH, PARALLEL_REPLICAS).

**Specific gotchas pinned to this problem (preserved in `IMPLEMENTATION_NOTES.md`):**
- `initial_address` must be a non-empty `host:port` string (e.g., `"127.0.0.1:0"`).
- `initial_time` is a fixed-width Int64 (8 bytes LE), **not** VarUInt — even though most other numeric fields in ClientInfo are VarUInt.

**Spec work:** §7.8.

---

#### Problem 13: Setting list ✅

`(String key, VarUInt flags, String value)` tuples in the Query body, terminated by an empty `key`. Flags: `0x01` = Important, `0x02` = Custom, `0x04` = Obsolete.

**Spec work:** §7.9.

---

#### Problem 14: Parameter list ✅

Encoded identically to settings with the `Custom` flag (`0x02`) set. Terminated by empty key. Parameter values must be **single-quoted on the wire** (`42` → `'42'`, `it's` → `'it''s'`); a bare value is silently dropped server-side. See `IMPLEMENTATION_NOTES.md` §1.9.

**Spec work:** §7.10.

---

#### Problem 15: External tables + end-of-client-data marker ✅

Zero or more external-table Data packets, followed by an **empty Data packet** (`table_name = ""`, `num_columns = 0`, `num_rows = 0`). The server does not begin executing the query until it receives this marker — even for SELECTs with no input data.

**Implementation:** `ExternalTable::encode_empty` writes the terminator. `QueryOptions::with_external_table` adds non-empty entries.

**Spec work:** §6.4 step 2, §7.11.

---

#### Problem 16: BlockInfo ✅

Field-tagged encoding: `[VarUInt field_id] [value]` repeating, terminated by `field_id = 0`. Field 1 = `is_overflows: UInt8`, field 2 = `bucket_number: Int32` (default `-1`, **not** `0`). Forward-compatible — unknown field IDs are skipped.

**Spec work:** §7.11 BlockInfo subsection.

---

#### Problem 17: Block decode + Column header ✅

Block: `[BlockInfo] [VarUInt: num_columns] [VarUInt: num_rows] [Column × num_columns]`. Column header: `[String: name] [String: type] [UInt8: has_custom_serialization (gated by CUSTOM_SERIALIZATION v54454)] [data]`. The `has_custom_serialization` byte must be present at v54454+ — omitting it misaligns every subsequent column by one byte.

**Spec work:** §7.11.

---

#### Problem 18: Response loop ✅

After the Query packet + EOI marker, the client loops reading response packets until `EndOfStream` or `Exception`. Dispatches by packet type code: Data (header / result / boundary marker), Progress, ProfileInfo, Totals, Extremes, Log, ProfileEvents, TableColumns, Exception, EndOfStream. Critical rule: **`num_rows == 0` is not a query terminator** — only `EndOfStream` (or `Exception`) ends the stream.

**Spec work:** §6.4 step 4.

---

#### Problem 19: Auxiliary response packets ✅

- **Progress** (cumulative metrics; not deltas — replace previous, do not sum).
- **ProfileInfo** (sent once near end of query).
- **Totals / Extremes** — same wire format as Data; carry a single Block.
- **Log** — Block with fixed schema of 8 columns (event_time, ..., text).
- **ProfileEvents** — Block with fixed schema of 6 columns. The `value` column's wire type varies between packets (Int64 vs UInt64 depending on the events being reported).
- **TableColumns** — `String external_table` + `String columns_description` (free-form text).
- **Exception** — error code + name + message + stack_trace + has_nested (chained exceptions).

All of these must be fully decoded (even if the value is discarded) so the stream position advances correctly.

**Spec work:** §7.12–§7.18.

---

### Phase 5: Basic data types ✅

The minimum viable type set for `SELECT 1`-style queries plus simple INSERTs.

#### Problem 20: Integer types — UInt8/16/32/64, Int8/32/64 ✅

Direct binary encodings of integer values. `bytes_per_value × num_rows` bytes per column, little-endian for everything except UInt8/Int8 (raw byte). Two's complement for signed.

**Implementation:** `ColumnData::Uint8(Vec<u8>)`, `Uint16`, `Uint32`, `Uint64`, `Int8`, `Int32`, `Int64` variants.

**Spec work:** §8.1.1.

---

#### Problem 21: DateTime ✅

Wire-compatible with UInt32 — Unix timestamp in seconds, 4 bytes LE. Type string may carry a timezone parameter (`DateTime('UTC')`, `DateTime('America/New_York')`); timezone is metadata only, not part of the wire value.

**Implementation:** strip the `(...)` parameter suffix to dispatch on the base type. `ColumnData::DateTime(Vec<u32>)`.

**Spec work:** §8.1.2.

---

#### Problem 22: Enum8 (alias for Int8) ✅

Wire-compatible with Int8. The full variant mapping lives in the type string (`Enum8('active' = 1, 'inactive' = 2)`); the wire bytes are identical to Int8.

**Implementation:** strip the `(...)` parameter suffix and dispatch as `Int8`. Round-tripping the type string verbatim is sufficient.

**Spec work:** §8.1.3.

---

#### Problem 23: String ✅

Sequence of `num_rows` length-prefixed byte strings: `[VarUInt: byte_length] [bytes]`. No row separators beyond the length prefixes. Empty strings are a single `0x00`. Embedded NUL is valid.

**Implementation:** `ColumnData::String(Vec<String>)`. UTF-8 validity is enforced on decode (acceptable for protocol-internal strings; can be relaxed if non-UTF-8 byte columns become a target use case).

**Spec work:** §8.2.1.

---

#### Problem 24: FixedString(N) ✅

Exactly `N × num_rows` raw bytes — no length prefixes. Server right-pads short values with NUL to `N` bytes; padding is part of the stored value and arrives on the wire. Treat as raw bytes, not text.

**Implementation:** parse `N` from the type string; `ColumnData::FixedString { n: usize, data: Vec<u8> }` where `data.len() == n × num_rows`.

**Spec work:** §8.2.2.

---

## Pending Work

Problems are sequenced so each one can be picked up independently. Each has a clear "done" criterion.

### Phase 6: Composite types (fixed shape) — §8.3 of the spec ✅

Types in this group have a stable unversioned wire format. Sub-stream layouts are known statically from the type string. **All problems in this phase are complete.**

#### Problem 25: `Nullable(T)` ✅

**Wire format:** `num_rows × UInt8` null-map (0 = present, 1 = null), then the inner type's encoding for all `num_rows` rows. Values at null positions are placeholder bytes that must still be consumed.

**Implementation:**
- Add `ColumnData::Nullable { inner: Box<ColumnData>, nulls: Vec<bool> }` (or similar).
- Parse type string `Nullable(InnerType)` to get the inner type.
- Recursive decode: null map, then inner.

**Tests:**
- Unit: `Nullable(UInt32)` with all nulls, no nulls, mixed.
- Integration: `SELECT CAST(NULL AS Nullable(UInt32))`, `SELECT arrayJoin([NULL::Nullable(UInt32), 42, 100])`.

**Spec work:**
- Fill in §8.3 with a concrete `Nullable` subsection including byte-level example.

**References:**
- ch-go: `proto/col_nullable.go`, `proto/col_nullable_of.go`
- ClickHouse: `src/DataTypes/Serializations/SerializationNullable.h`, `SerializationNullable.cpp`

---

#### Problem 26: `Array(T)` ✅

**Wire format:** `num_rows × UInt64` cumulative end-offsets, then `offsets[num_rows - 1]` values of the inner type.

**Implementation:**
- Add `ColumnData::Array { inner: Box<ColumnData>, offsets: Vec<u64> }`.
- Parse type string `Array(InnerType)`.
- Recursive — inner type may itself be composite.

**Tests:**
- Unit: empty array, array-of-integers, nested `Array(Array(T))`.
- Integration: `SELECT [1, 2, 3]`, `SELECT arrayJoin([[1,2], [], [3,4,5]])`.

**Spec work:**
- §8.3 `Array` subsection with a byte-level example that includes a concrete offsets table.

**References:**
- ch-go: `proto/col_arr.go`, `proto/col_arr_go123.go`
- ClickHouse: `src/DataTypes/Serializations/SerializationArray.h`, `SerializationArray.cpp`, `SerializationArrayOffsets.cpp`

---

#### Problem 27: `Tuple(T1, T2, ...)` ✅

**Wire format:** each element type encoded as a separate stream of `num_rows` values, concatenated in declaration order.

**Implementation:**
- Type-string parser must split the parenthesized element list (respecting nested parens).
- `ColumnData::Tuple(Vec<ColumnData>)`.

**Tests:**
- Unit: heterogeneous tuple `Tuple(UInt8, String)`, nested `Tuple(Tuple(...))`.
- Integration: `SELECT (1, 'hello', 3.14)`.

**Spec work:** §8.3 `Tuple` subsection.

**References:**
- ch-go: `proto/col_tuple.go`
- ClickHouse: `src/DataTypes/Serializations/SerializationTuple.h`, `SerializationTuple.cpp`

---

#### Problem 28: `Map(K, V)` ✅

**Wire format:** equivalent to `Array(Tuple(K, V))` — one offsets stream + a paired values stream.

**Implementation:**
- Parse `Map(KeyType, ValueType)`.
- Decode as if it were `Array(Tuple(K, V))`.
- Expose as a map/dict in the user API.

**Tests:**
- Integration: `SELECT map('a', 1, 'b', 2)`.

**Spec work:** §8.3 `Map` subsection.

**References:**
- ch-go: `proto/col_map.go`
- ClickHouse: `src/DataTypes/Serializations/SerializationMap.h`, `SerializationMap.cpp`

---

#### Problem 29: `Nested(...)` ✅

Behavior depends on the server-side `flatten_nested` setting. With `flatten_nested = 1` (default), `Nested(a T1, b T2)` becomes parallel `Array(T_i)` columns with dotted names (`n.a Array(T1)`, `n.b Array(T2)`) — handled by the existing Array decoder. With `flatten_nested = 0`, the column appears on the wire with type string `Nested(...)` and is byte-identical to `Array(Tuple(...))` after the type string. The implementation supports both, including a dedicated `ColumnData::Nested { fields: Vec<(String, ColumnData)>, offsets }` variant for the `flatten_nested = 0` case.

**Tests:** unit tests for the Nested encoding plus integration tests using `::Nested(...)` casts (which reach the `flatten_nested = 0` shape without DDL).

**Spec work:** §8.3 `Nested` subsection documenting both cases. ✅

**References:**
- ch-go: uses `Array` + naming convention (no dedicated `col_nested.go`)
- ClickHouse: `src/DataTypes/DataTypeNested.h`, `DataTypeNested.cpp` (registration); `src/DataTypes/NestedUtils.h` (flattening logic)

---

### Phase 7: More fixed-width and parameterized types — §8.1 ✅

Fixed-width types with parameter parsing or special encoding. **All problems in this phase are complete.** Time/Time64 deferred per server reality (resolves to Int64 on this server version).

#### Problem 30: Remaining integer types ✅

- `Int16` (2 bytes LE signed) — add to ColumnData and match list.
- `Float32` (4 bytes, IEEE 754 LE).
- `Float64` (8 bytes, IEEE 754 LE).
- `Bool` (1 byte, 0/1, domain over UInt8).

**Spec work:** move these from §8.1.4 "not yet implemented" to the main §8.1 table.

**References:**
- ch-go: `proto/col_int16_gen.go`, `proto/col_float32_gen.go`, `proto/col_float64_gen.go`, `proto/col_bool.go`
- ClickHouse: `src/DataTypes/DataTypeNumberBase.cpp`, `src/DataTypes/DataTypesNumber.cpp`, `src/DataTypes/Serializations/SerializationNumber.cpp`, `src/DataTypes/Serializations/SerializationBool.cpp`, `src/DataTypes/DataTypeDomainBool.cpp`

---

#### Problem 31: `Date`, `Date32`, `DateTime64`, `Time`, `Time64` ⚠️ (Time/Time64 deferred)

Date/time family. All fixed-width; scale/timezone parameters live in the type string and affect interpretation, not bytes.

- `Date` — 2 bytes, UInt16 days since `1970-01-01`.
- `Date32` — 4 bytes, Int32 days since `1970-01-01` (allows pre-1970).
- `DateTime64(scale)` or `DateTime64(scale, 'UTC')` — 8 bytes, Int64 ticks at the given scale (0..9). Scale 3 = ms, 6 = µs, 9 = ns.
- `Time` — 4 bytes, Int32 representing an hours/minutes/seconds duration (range spans centuries). Unlike `Date` it is a **duration**, not a calendar date. Added in a recent server version; encountered during integration tests against recent ClickHouse.
- `Time64(scale)` — 8 bytes, Int64 ticks at the given scale. Same scale semantics as `DateTime64`. Added alongside `Time`.

**Implementation:**
- Parse scale from the type string for `DateTime64` and `Time64`.
- Strip timezone parameter if present (same base-type stripping rule as §11.9 in the spec).
- Return decoded values as structured types if a library is available; otherwise raw integers + scale metadata.

**Spec work:** subsections in §8.1 with byte-level examples. Note in §11 the distinction between `DateTime*` (absolute instant in time) and `Time*` (duration / time-of-day-like value).

**References:**
- ch-go: `proto/col_date_gen.go`, `proto/col_date32_gen.go`, `proto/col_datetime64.go` (no ch-go Time/Time64 — added after ch-go's coverage cap)
- ClickHouse: `src/DataTypes/DataTypeDate.cpp`, `DataTypeDate32.cpp`, `DataTypeDateTime64.cpp`, `DataTypeTime.cpp`, `DataTypeTime64.cpp`, `src/DataTypes/Serializations/SerializationDate.cpp`, `SerializationDate32.cpp`, `SerializationDateTime64.cpp`, `SerializationTime.cpp`, `SerializationTime64.cpp`
- Registered in `DataTypeFactory.cpp:344` (`registerDataTypeTime(*this)`)

---

#### Problem 32: `UUID` ✅

**Wire format:** 16 bytes, transmitted as **two little-endian UInt64 halves, each byte-swapped** (historical quirk tied to ClickHouse issue #34369).

**Implementation:**
- Read 16 bytes, byte-swap the two halves to get the canonical form.
- Use `uuid` crate for the value type.

**Tests:**
- Unit: encode/decode known UUID, verify byte order.
- Integration: `SELECT toUUID('550e8400-e29b-41d4-a716-446655440000')` and match bytes.

**Spec work:** §8.1 `UUID` subsection + §11 implementation note about the byte-swap quirk.

**References:**
- ch-go: `proto/col_uuid.go`
- ClickHouse: `src/DataTypes/DataTypeUUID.cpp`, `src/DataTypes/Serializations/SerializationUUID.cpp`

---

#### Problem 33: `IPv4`, `IPv6` ✅

- `IPv4` — 4 bytes.
- `IPv6` — 16 bytes (FixedString(16)-compatible).

**Spec work:** §8.1 subsections with examples.

**References:**
- ch-go: `proto/col_ipv4.go`, `proto/col_ipv6.go`
- ClickHouse: `src/DataTypes/DataTypeIPv4andIPv6.cpp`

---

#### Problem 34: `Enum16` ✅

Wire-compatible with `Int16` (2 bytes LE signed). Same principle as `Enum8` (§11.8 in spec): variant labels live in the type string, byte layout is Int16.

**Tests:**
- Unit: encode/decode a small Enum16.
- Integration: `SELECT CAST(1 AS Enum16('a' = 1, 'b' = 30000))`.

**References:**
- ch-go: `proto/col_enum.go`
- ClickHouse: `src/DataTypes/DataTypeEnum.cpp`, `src/DataTypes/Serializations/SerializationEnum.cpp`

---

#### Problem 35: `Decimal(P, S)` and `Decimal32/64/128/256` ✅

**Wire format:** 4/8/16/32 bytes LE signed integer representing `value * 10^S` where `S` is the scale.

**Implementation:**
- Parse `(P, S)` from type string.
- Store raw integer + scale; let caller interpret.

**Spec work:** §8.1 `Decimal` subsection.

**References:**
- ch-go: `proto/col_decimal32_gen.go`, `proto/col_decimal64_gen.go`, `proto/col_decimal128_gen.go`, `proto/col_decimal256_gen.go`
- ClickHouse: `src/DataTypes/DataTypesDecimal.cpp`, `src/DataTypes/Serializations/SerializationDecimal.cpp`, `SerializationDecimalBase.cpp`

---

#### Problem 36: `Int128`, `UInt128`, `Int256`, `UInt256` ✅

Straight-up 16 or 32 byte little-endian two's-complement integers.

**Spec work:** §8.1 subsection.

**References:**
- ch-go: `proto/col_int128_gen.go`, `proto/col_uint128_gen.go`, `proto/col_int256_gen.go`, `proto/col_uint256_gen.go`
- ClickHouse: `src/DataTypes/DataTypesNumber.cpp`, `src/DataTypes/Serializations/SerializationNumber.cpp`

---

### Phase 8: Versioned/stateful types — §8.4 ⚠️

Implementation effort jumps significantly here. Each of these types has a serialization-version prefix and may maintain cross-block state. **Status:** LowCardinality (single-block) and JSON Tier 1 (String fallback) implemented; Variant, Dynamic, and JSON Tier 2/3 deferred — see §8.4.5 of `NATIVE_FORMAT.md` for the rationale.

#### Problem 37: `LowCardinality(T)` — simplest of the versioned types ⚠️ (single-block; multi-block pending)

**Wire format:**
- State prefix: `Int64(1)` — `key_serialization_version = sharedDictionariesWithAdditionalKeys` (only defined value).
- Per block: `Int64` metadata (flags + key type: UInt8/16/32/64), `Int64` dict_size, dict values, `Int64` keys_count, keys.
- Cross-block state: the dictionary accumulates across blocks (only new keys are sent per block).

**Implementation:**
- New `ColumnData::LowCardinality { inner: Box<ColumnData>, dict: Vec<...>, keys: Vec<u32> }` or similar.
- Per-block state held on the column decoder.

**Spec work:** §8.4 subsection — this is the simplest versioned type and a good first example.

**References:**
- ch-go: `proto/col_low_cardinality.go`
- ClickHouse: `src/DataTypes/DataTypeLowCardinality.cpp`, `src/DataTypes/Serializations/SerializationLowCardinality.h`, `SerializationLowCardinality.cpp`
- clickhouse-go: `lib/column/lowcardinality.go`

---

#### Problem 38: `Variant(T1, T2, ...)` ❌ Deferred

**Wire format:**
- State prefix: `UInt64 LE` discriminators mode (0=BASIC, 1=COMPACT).
- Per block:
  - BASIC: `num_rows × UInt8` discriminators; then each sub-column's values.
  - COMPACT: per-granule marker + optimized encoding.
- `NULL_DISCRIMINATOR = 255`.

**Implementation:**
- Parse type string to get variant type list.
- Decode discriminators, dispatch each row to the right sub-column.

**Spec work:** §8.4 subsection with BASIC mode example first.

**References:**
- ClickHouse: `src/DataTypes/DataTypeVariant.cpp`, `src/DataTypes/Serializations/SerializationVariant.h`, `SerializationVariant.cpp`, `SerializationVariantElement.cpp`
- clickhouse-go: `lib/column/variant.go`
- ch-go: no direct implementation (ch-go predates Variant)

---

#### Problem 39: `Dynamic` ❌ Deferred

**Wire format:**
- State prefix: `UInt64 LE` serialization version (V1=1, V2=2, V3=4, FLATTENED=3).
- Then list of runtime-discovered variant type names.
- Then the `Variant` encoding using those types.
- Cross-block state: type list grows across blocks.

**Implementation:**
- Requires `Variant` support first (Problem 38).
- Must handle the type list growing across blocks within a query response.

**Spec work:** §8.4 subsection including version-dispatch logic.

**References:**
- ClickHouse: `src/DataTypes/DataTypeDynamic.cpp`, `src/DataTypes/Serializations/SerializationDynamic.h`, `SerializationDynamic.cpp`, `SerializationDynamicElement.cpp`, `SerializationDynamicHelpers.cpp`
- clickhouse-go: `lib/column/dynamic.go`, `lib/column/dynamic_deprecated.go`, `lib/column/sharedvariant.go`
- ch-go: no direct implementation (ch-go predates Dynamic)

---

#### Problem 40: `JSON` (Tier 1: String fallback only) ✅

**Wire format when version = 1 (STRING mode):**
- `UInt64 LE(1)` — `JSONStringSerializationVersion`.
- Regular `String` column encoding of `num_rows` JSON text payloads.

**Implementation:**
- Auto-inject `output_format_native_write_json_as_string = 1` into query settings.
- Detect `JSON` type string, check version byte is `1`, decode remaining as String.
- Reject other version values with a clear error.

**Why start here:** this unblocks JSON-returning queries for minimal effort (hours, not weeks).

**Spec work:** §8.4 `JSON` subsection explaining Tier 1 strategy; cross-reference the implementation-tier breakdown already in §8.4.2.1.

**References:**
- ch-go: `proto/col_json_str.go` (exactly this approach)
- clickhouse-go: `lib/column/json.go` (handles both Tier 1 and Tier 2 — see `JSONStringSerializationVersion` branch around lines 967–996)

---

#### Problem 41: `JSON` (Tier 2: FLATTENED mode) ❌ Deferred

**Wire format when version = 3 (FLATTENED) or version = 0 (deprecated, auto-upgraded):**
- `UInt64 LE` serialization version.
- Path list (dynamic paths discovered server-side).
- Per path: a `Dynamic` column encoding.
- Shared data column at the end.

**Implementation:** requires Problems 37 (LowCardinality state-prefix handling helpers), 38 (Variant), 39 (Dynamic) first. This is thousands of lines of recursive decoding.

**Skip if:** Tier 1 is sufficient for target users. Most real clients (ch-go, clickhouse-go v2) took this on; toy clients do not need it.

**Spec work:** §8.4 extended `JSON` coverage with FLATTENED byte-level walkthrough.

**References:**
- ClickHouse: `src/DataTypes/DataTypeObject.h`, `DataTypeObject.cpp`, `src/DataTypes/Serializations/SerializationJSON.h`, `SerializationJSON.cpp`, `src/DataTypes/Serializations/SerializationObject.h`, `SerializationObject.cpp`, `SerializationObjectDistinctPaths.cpp`, `SerializationObjectDynamicPath.cpp`, `SerializationObjectSharedData.cpp`
- clickhouse-go: `lib/column/json.go`, `lib/column/json_reflect.go`, `lib/column/json_deprecated.go` (the production reference implementation)
- ch-go: does not implement Tier 2

---

### Phase 9: Compression — §9 of spec ⚠️

Frame primitives (LZ4, ZSTD, NONE; CityHash102 checksum verification; corruption detection) are implemented. Connection-level integration — wrapping the inner stream's Block reads/writes when `compression = true` is requested — is not yet wired up.

#### Problem 42: LZ4 compression ⚠️ (frame primitives done; connection integration pending)

**Frame format per block:**
- 16 bytes — CityHash128 checksum (over everything that follows).
- 1 byte — method byte: `0x82` = LZ4, `0x90` = ZSTD, `0x02` = NONE.
- 4 bytes LE — compressed size (including the 9-byte header but not the checksum).
- 4 bytes LE — uncompressed size.
- N bytes — compressed data.

Activated by the `compression` flag in the Query packet.

**Implementation:**
- Add `cityhash` and `lz4` Rust crate dependencies.
- Wrap the block reader/writer with a compression framing layer.
- Verify checksums on read; fail loudly on mismatch.

**Spec work:** replace §9 placeholder with full frame format and checksum algorithm.

**References:**
- ch-go: `compress/reader.go`, `compress/writer.go`, `compress/compress.go`
- ClickHouse: `src/Compression/CompressionInfo.h`, `src/Compression/CompressedWriteBuffer.h/cpp`, `src/Compression/CompressedReadBuffer.h/cpp`, `src/Compression/CompressionCodecLZ4.cpp`, `base/pocoext/CityHash.h`

---

#### Problem 43: ZSTD compression ⚠️ (same status as Problem 42)

Same frame format, method byte `0x90`. Uses `zstd` crate. Spec work already covered by Problem 42.

**References:**
- ch-go: `compress/reader.go` (ZSTD branch)
- ClickHouse: `src/Compression/CompressionCodecZSTD.cpp`

---

### Phase 10: INSERT path — §6 (new state machine) ✅

INSERT phase implemented end-to-end with both single-block and multi-block (streaming) APIs. Schema-block exchange, end-of-input terminator, and metadata-packet drain all working. See `NATIVE_PROTOCOL.md` §5.5 for the spec.

#### Problem 44: INSERT with a single block ✅

Currently `query()` only handles SELECT-style responses. INSERT flow is:

1. Client sends Query(`INSERT INTO t VALUES`).
2. Server responds with a Data packet containing the **schema block** (0 rows, describes expected columns).
3. Client sends one or more Data packets with actual rows.
4. Client sends the empty Data marker.
5. Server responds with EndOfStream (or Exception).

**Implementation:**
- New `insert(sql, block)` API or extend `query_with` with an "insert_data" option.
- Encode outbound Data packets using Column::encode (already implemented).

**Spec work:** add §6.5 INSERT Phase with its own state diagram.

**References:**
- ch-go: `insert.go`, `proto/block.go:152-184` (`EncodeRawBlock`), `proto/client_data.go`
- ClickHouse: `src/Server/TCPHandler.cpp` (`processInsertQuery`, `receivePacketsExpectData`), `src/Client/Connection.cpp` (`sendData`, `sendExternalTablesData`)

---

#### Problem 45: Streaming INSERT (multiple blocks) ✅

Extension of Problem 44. Let the caller push blocks incrementally.

**References:**
- ch-go: same as Problem 44
- clickhouse-go: `lib/driver/stdlib/stmt.go` (streaming batch pattern)

---

### Phase 11: Bring spec up to server v54483 — new feature sections ⚠️

Document the features added between v54460 (current spec's cap) and v54483. Each feature goes into `NATIVE_PROTOCOL.md` §3.3 (feature table); most also add fields to existing message types.

#### Problem 46: v54461 — password complexity rules ✅

ServerHello gets: `VarUInt rules_count` + `count × (String pattern, String message)` if client ≥ 54461.

**Implementation:** new `Feature::PASSWORD_COMPLEXITY_RULES` constant; `ServerHello.password_complexity_rules: Option<Vec<(String, String)>>` field with feature-gated encode/decode; decoder enforces a 256-rule cap (matching `DBMS_MAX_PASSWORD_COMPLEXITY_RULES`) before any allocation; client's declared max protocol bumped from `54459` (PARAMETERS) to `54461`. Per-string 4096-byte cap (`DBMS_MAX_HELLO_STRING_SIZE`) deferred — it requires a capped `read_string` variant in `wire.rs` and the same cap should apply consistently to every handshake-time string, not just these two.

**Spec work done:** `NATIVE_PROTOCOL.md` §3.3 (feature table row) + §6.2 (ServerHello field 8 plus a Rule sub-table and a paragraph specifying the SHOULD caps and the advisory semantics). `IMPLEMENTATION_NOTES.md` §1.11 covers the bounded-decode hazard and the per-string cap rationale.

**References:**
- ClickHouse: `src/Server/TCPHandler.cpp:2131-2160` (`sendHello`); `src/Client/Connection.cpp:610-633` (receive). Feature flag `DBMS_MIN_PROTOCOL_VERSION_WITH_PASSWORD_COMPLEXITY_RULES = 54461` in `src/Core/ProtocolDefines.h:85`. Cap constants in the same file at lines 92 and 100.
- ch-go: not implemented (v54460 cap).

---

#### Problem 47: v54462 — inter-server secret v2 nonce

ServerHello gets: `Int64 LE nonce` (8 bytes **fixed**, not VarUInt) if client ≥ 54462.

**Spec work:** §7.2 + §4.3 + §11 note about the fixed-width encoding.

**References:**
- ClickHouse: `DBMS_MIN_REVISION_WITH_INTERSERVER_SECRET_V2 = 54462` in `src/Core/ProtocolDefines.h`; usage in `src/Server/TCPHandler.cpp::sendHello` and `src/Client/Connection.cpp::receiveHello`

---

#### Problem 48: v54463 — total_bytes_to_read in Progress

Progress packet gets `VarUInt total_bytes_to_read` as a new field. Gated by `DBMS_MIN_PROTOCOL_VERSION_WITH_TOTAL_BYTES_IN_PROGRESS = 54463`.

**Spec work:** §7.12 Progress table + §4.3.

**References:**
- ClickHouse: `src/IO/Progress.cpp`, `src/IO/Progress.h`; feature flag at `src/Core/ProtocolDefines.h:89`

---

#### Problem 49: v54464 — TimezoneUpdate packet (server → client)

New ServerPacket::TimezoneUpdate (code `17`, already in enum). Body: single String carrying the server's timezone after a change (e.g., `SET timezone = '...'`).

**Spec work:** §7.x new subsection + §6.4 query-phase dispatch table + §4.3.

**References:**
- ClickHouse: `src/Server/TCPHandler.cpp` (`sendTimezone`); feature flag at `src/Core/ProtocolDefines.h:91`

---

#### Problem 50: v54465 — sparse serialization

The `has_custom_serialization` byte in Column header (§7.11) can now be `1` for sparse encoding. Clients that declared v ≥ 54465 must handle this or decline.

**Spec work:** §7.11 Column subsection — document the sparse variant's kind_stack contents + §11 note.

**References:**
- ClickHouse: `src/DataTypes/Serializations/SerializationSparse.h`, `SerializationSparse.cpp`; feature flag at `src/Core/ProtocolDefines.h:93`; `src/DataTypes/Serializations/SerializationInfo.h` (kind_stack serialization)
- clickhouse-go: handled transparently via `Column.WriteStatePrefix` / `ReadStatePrefix`

---

#### Problem 51: v54466 — SSH authentication

New challenge-response auth flow:
- ClientPacket::SSHChallengeRequest (`11`)
- ServerPacket::SSHChallenge (`18`) — single String challenge
- ClientPacket::SSHChallengeResponse (`12`) — signature String

**Spec work:** new §7 subsections for all three packets + §6 new alternative handshake path + §4.3.

**References:**
- ClickHouse: `src/Server/TCPHandler.cpp` (`receiveSSHChallenge`, `sendSSHChallenge`); feature flag at `src/Core/ProtocolDefines.h:95`
- clickhouse-go: `lib/auth/ssh.go`

---

#### Problem 52: v54469 — rows_before_aggregation in ProfileInfo

ProfileInfo gets two new fields: `Bool applied_aggregation` + `VarUInt rows_before_aggregation`. Already in code (`ROWS_BEFORE_AGGREGATION = 54469` feature).

**Spec work:** already covered in §7.13 — verify it's accurate.

**References:**
- ClickHouse: `src/QueryPipeline/ProfileInfo.cpp`, `ProfileInfo.h`; feature flag at `src/Core/ProtocolDefines.h:102`

---

#### Problem 53: v54470 — chunked protocol

**The biggest v54460+ change.** Each packet on the wire becomes:
- `4 bytes LE chunk size` + packet bytes + `4 bytes zero terminator`.
- Large packets may split across multiple chunks.

Negotiated in Addendum: `String proto_send_chunked` + `String proto_recv_chunked`, each one of `"chunked"`, `"notchunked"`, `"chunked_optional"`, `"notchunked_optional"`.

**Implementation:** wrapping layer on top of the TCP stream. Buffer N bytes, prefix chunk header, append zero terminator.

**Spec work:** major addition. New §5.x subsection on chunked framing; update §5 Packet Envelope to note chunked mode; update Addendum (§7.3) with the two new fields; §4.3 feature entry.

**References:**
- ClickHouse: `src/IO/ReadBufferFromPocoSocketChunked.h/cpp`, `src/IO/WriteBufferFromPocoSocketChunked.h/cpp`; negotiation in `src/Server/TCPHandler.cpp::runImpl` (lines ~405-445) and `src/Client/Connection.cpp::connect` (lines ~299-334); feature flag at `src/Core/ProtocolDefines.h:105`
- clickhouse-go: `lib/proto/chunked.go` (proxy stream), `lib/proto/client.go` (negotiation)

---

#### Problem 54: v54471 — versioned parallel replicas protocol

Addendum gets `VarUInt parallel_replicas_protocol_version`. ServerHello mirrors this field. Client-relevant only if participating in distributed queries (most clients set it to the server's current default: 6).

**Spec work:** §7.2 + §7.3 + §4.3.

**References:**
- ClickHouse: `src/Core/ProtocolDefines.h:45-50` (PARALLEL_REPLICAS_*_VERSION constants); flag at line 107; usage in `src/Server/TCPHandler.cpp` (`sendMergeTreeAllRangesAnnouncement`, `sendMergeTreeReadTaskRequest`)

---

#### Problem 55: v54473 — V2 Dynamic and JSON serialization

Introduces `Dynamic::V2` and `Object::V2` variants. Affects the serialization-version table (§8.4.2) — already documented but verify correctness.

**References:**
- ClickHouse: `src/DataTypes/Serializations/SerializationDynamic.h/cpp` (V1→V2 branch), `SerializationObject.h/cpp`; feature flag `DBMS_MIN_REVISION_WITH_V2_DYNAMIC_AND_JSON_SERIALIZATION = 54473` in `src/Core/ProtocolDefines.h`

---

#### Problem 56: v54474 — server settings

ServerHello gets a full settings list (same format as Query.settings, terminated by empty-string key) after the handshake metadata. Servers broadcast their non-default settings so the client can log or display them.

**Spec work:** §7.2 ServerHello subsection — new trailing field.

**References:**
- ClickHouse: `src/Server/TCPHandler.cpp::sendHello` (settings block at the tail); feature flag `DBMS_MIN_REVISION_WITH_SERVER_SETTINGS = 54474`

---

#### Problem 57: v54475 — script query/line numbers (NOT JSON)

ClientInfo gets `VarUInt script_query_number` + `VarUInt script_line_number` for multi-statement error reporting.

**Spec work:** §7.8 ClientInfo table — add two fields at the end, gated by new Feature `QUERY_AND_LINE_NUMBERS = 54475` (not `JSONStrings`).

**References:**
- ClickHouse: `src/Interpreters/ClientInfo.cpp:142-146` (encode), `:249-253` (decode); feature flag `DBMS_MIN_REVISION_WITH_QUERY_AND_LINE_NUMBERS = 54475`

---

#### Problem 58: v54476 — JWT auth in interserver

Extends authentication modes; only relevant for server-to-server. Document briefly as inter-server only.

**Spec work:** §4.3 + a paragraph in §3.2.

**References:**
- ClickHouse: `DBMS_MIN_REVISON_WITH_JWT_IN_INTERSERVER = 54476` (note the typo in the ClickHouse constant name: "REVISON"); usage in `src/Interpreters/ClientInfo.cpp` JWT field

---

#### Problem 59: v54477 — query plan serialization

Adds:
- ClientPacket::QueryPlan (`13`) — client can ship a pre-built query plan.
- ServerHello adds `VarUInt query_plan_serialization_version`.

Rare for external clients; primarily for inter-server distributed execution.

**Spec work:** §7.2 ServerHello + §10.1 packet type table.

**References:**
- ClickHouse: `src/Processors/QueryPlan/QueryPlan.cpp::serialize`, `::deserialize`; feature flag `DBMS_MIN_REVISION_WITH_QUERY_PLAN_SERIALIZATION = 54477`; usage in `src/Client/Connection.cpp::sendQueryPlan`

---

#### Problem 60: v54478 — parallel block marshalling / binary type encoding

Types may be transmitted in a compact binary form instead of type-name strings. Gated by a block-level flag.

**Spec work:** §7.11 Column — note the binary type encoding alternative.

**References:**
- ClickHouse: `src/DataTypes/DataTypesBinaryEncoding.h/cpp` (binary type IDs), `src/Formats/NativeWriter.cpp` (branch on `format_settings.native.encode_types_in_binary_format`); feature flag `DBMS_MIN_REVISON_WITH_PARALLEL_BLOCK_MARSHALLING = 54478`

---

#### Problem 61: v54479 — versioned cluster function protocol

ServerHello gets another VarUInt version number. Mostly inter-server.

**References:**
- ClickHouse: `DBMS_MIN_REVISION_WITH_VERSIONED_CLUSTER_FUNCTION_PROTOCOL = 54479`; `src/Server/TCPHandler.cpp::sendHello` (appended VarUInt)

---

#### Problem 62: v54480 — out-of-order buckets in aggregation

BlockInfo gets extended for aggregation bucketing. Server-internal in most cases, but BlockInfo's field-tagged encoding gracefully handles new fields — the existing decoder skips unknown field IDs.

**Spec work:** verify §7.11 BlockInfo note about forward compatibility.

**References:**
- ClickHouse: `src/Core/BlockInfo.h`, `BlockInfo.cpp` (field 3 / `out_of_order_buckets`); feature flag `DBMS_MIN_REVISION_WITH_OUT_OF_ORDER_BUCKETS_IN_AGGREGATION = 54480`

---

#### Problem 63: v54481 — compressed logs/profile_events blocks

Log (§7.16) and ProfileEvents (§7.17) packets may now wrap their block body in the compression frame (§9). Previously only Data/Totals/Extremes compressed.

**Spec work:** §7.16 and §7.17 — note the potential compression framing.

**References:**
- ClickHouse: `src/Server/TCPHandler.cpp::sendLogs` / `::sendProfileEvents` (compression branch); feature flag `DBMS_MIN_REVISION_WITH_COMPRESSED_LOGS_PROFILE_EVENTS_COLUMNS = 54481`

---

#### Problem 64: v54482 — replicated serialization

For `ReplicatedMergeTree` tables, certain columns may carry replication metadata. Mostly inter-server.

**References:**
- ClickHouse: `DBMS_MIN_REVISION_WITH_REPLICATED_SERIALIZATION = 54482`; `src/DataTypes/Serializations/SerializationReplicated.h/cpp`

---

#### Problem 65: v54483 — nullable sparse serialization

Extends the sparse-serialization feature from v54465 to `Nullable(T)` columns. Requires the `Nullable` implementation (Problem 25) to cooperate with sparse decoding.

**References:**
- ClickHouse: `DBMS_MIN_REVISION_WITH_NULLABLE_SPARSE_SERIALIZATION = 54483`; extends `SerializationNullable.cpp` + `SerializationSparse.cpp` composition

---

### Phase 12: Polish and presentation ⏳

#### Problem 66: Client-side TCP keepalive

Set `SO_KEEPALIVE` + `TCP_KEEPIDLE=290` on the TCP socket using the `socket2` crate. Currently relies on OS defaults.

**Spec work:** already documented in §12.1.1 that this is asymmetric and client-side-only.

**References:**
- ClickHouse: `src/Client/Connection.cpp:263-276` (client-side setKeepAlive + TCP_KEEPIDLE)

---

#### Problem 67: BufReader / BufWriter

Wrap the TcpStream with application-level buffering to reduce syscall count. Per §12 discussion, chunked protocol would be the "real" solution but a BufWriter covers 90% of the pathological cases today.

**References:**
- ClickHouse: `src/IO/BufferBase.h`, `WriteBuffer.h`, `ReadBuffer.h` (the server's buffering framework)

---

#### Problem 68: Public API polish

- Error types: switch from `io::Error` to a typed `Error` enum (e.g., `ProtoError::Io`, `ProtoError::Server(ServerException)`, `ProtoError::UnsupportedType`, etc.).
- Module re-exports: `pub use proto::query::Stage;` at the crate root, etc.
- Documentation comments on all public items.
- `#[non_exhaustive]` on enum variants that will grow (ServerPacket, ClientPacket, ColumnData, ...).

**References:** N/A — project-internal API design.

---

#### Problem 69: Benchmark vs. ch-go and clickhouse-go

Criterion-style benchmarks for: VarUInt read/write, Block decode, end-to-end query latency for `SELECT 1` and `SELECT * FROM system.numbers LIMIT 1000`.

**References:**
- ch-go: `internal/cmd/ch-bench/` (workload harness)
- clickhouse-go: `benchmark/native/benchmark_test.go`

---

### Phase 13: Spec completion ⚠️

Now lives in the three split documents (`NATIVE_FORMAT.md`, `NATIVE_PROTOCOL.md`, `IMPLEMENTATION_NOTES.md`) instead of a single `SPEC.md`.

#### Problem 70: Fill composite types section ✅

Done as part of Phase 6. `NATIVE_FORMAT.md` §3.3 covers Nullable, Array, Tuple, Map, Nested with byte-level examples each.

---

#### Problem 71: Fill versioned types section ⚠️

Done in part. `NATIVE_FORMAT.md` §3.4 covers LowCardinality and JSON Tier 1 with byte-level examples; Variant, Dynamic, JSON Tier 2/3 documented as "out of scope for this revision" (§3.4.5) with rationale, pending Problems 38–41.

---

#### Problem 72: Replace compression section placeholder ✅

Done. `NATIVE_FORMAT.md` §4 covers the frame format, method bytes, CityHash102 checksum, per-block boundaries, and negotiation.

---

#### Problem 73: Add chunked-protocol section ⏳

Post-Problem 53. Major structural addition to `NATIVE_PROTOCOL.md` §4 (Packet Envelope) and §5 (Connection Lifecycle).

---

## Verification Strategy

For every problem:

1. **Unit tests** — encode/decode roundtrips with known byte sequences. Run with `cargo test`.
2. **Integration tests** — `make test-integration` runs against a live ClickHouse via docker-compose.
3. **Packet-level comparison** — when in doubt, compare byte-for-byte output against ch-go or clickhouse-go for the same query.
4. **Spec-then-code** — for new types, write the spec entry first, then implement against the spec, then verify the spec matches real server behavior.

---

## Key Files Quick Reference

| What | This project | ClickHouse server |
|------|--------------|-------------------|
| Entry point | `src/lib.rs` | — |
| Connection + Query + INSERT | `src/client.rs` | `src/Client/Connection.cpp`, `src/Server/TCPHandler.cpp` |
| Wire primitives | `src/proto/wire.rs` | `src/IO/VarInt.h`, `src/IO/WriteHelpers.h`, `src/IO/ReadHelpers.h` |
| Feature constants | `src/proto/feature.rs` | `src/Core/ProtocolDefines.h` |
| Packet enums | `src/proto/packet.rs` | `src/Core/Protocol.h` |
| Block/Column | `src/proto/block.rs`, `src/proto/column.rs` | `src/Formats/NativeWriter.cpp`, `src/Formats/NativeReader.cpp`, `src/DataTypes/` |
| Compression | `src/proto/compression.rs` | `src/Compression/CompressionInfo.h`, `src/Compression/CompressedReadBuffer.h/cpp`, `src/Compression/CompressedWriteBuffer.h/cpp` |
| Query options | `src/options.rs` | `src/Core/Settings.cpp` |
| Examples | `examples/events.rs`, `examples/catalog.rs` | — |
| Spec — protocol | `NATIVE_PROTOCOL.md` | — |
| Spec — format | `NATIVE_FORMAT.md` | — |
| Implementation notes | `IMPLEMENTATION_NOTES.md` | — |
| Spec entry point | `SPEC.md` (redirect) | — |

---

## Scope boundaries (explicit non-goals)

- **HTTP interface** — out of scope. TCP native only. Also no other grpc mechanisms
- **Server-side implementation** — out of scope. Client only, with server behavior reverse-engineered for spec.
- **JSON Tier 3 (V3 on-disk format)** — not targeted; see §8.4.2.1 in spec.
- **Multi-threaded / async I/O** — out of scope. Single-threaded blocking I/O; `tokio` version is a separate future project.
- **TLS** — not implemented yet but noted in §3.1 as a transport-layer concern. Planned for later. 
- **Inter-server mode** — out of scope. Client-to-server only.
