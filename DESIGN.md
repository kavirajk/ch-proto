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

## Current State (as of latest work)

### Implementation — what works

**Client declares protocol:** `54459` (Feature::PARAMETERS). Negotiated with server yields the actual working version.

**Connection lifecycle (fully wired):**
- TCP connect with `TCP_NODELAY` (via default `TcpStream`), `SO_KEEPALIVE` and `TCP_KEEPIDLE` **not yet applied client-side**.
- Handshake (ClientHello ↔ ServerHello with version negotiation).
- Addendum (sends empty quota_key when negotiated version ≥ 54458).
- Ping/Pong.
- Query with full response loop (handles Data, Progress, ProfileInfo, Totals, Extremes, Log, ProfileEvents, TableColumns, EndOfStream, Exception).

**Query options (builder pattern `QueryOptions`):**
- `with_query_id`, `with_stage`, `with_compression`, `with_setting`, `with_param`, `with_external_table`
- `query()` delegates to `query_with(sql, QueryOptions::new())`.

**Column types implemented:**
- Fixed-width: `UInt8`, `UInt16`, `UInt32`, `UInt64`, `Int8`, `Int32`, `Int64`, `DateTime`, `Enum8` (alias for Int8)
- Variable-length: `String`, `FixedString(N)`

**QueryResult structure:** exposes header (schema), result rows (Vec<Block>), totals, extremes, profile, logs, profile_events.

**Test coverage:** 121 unit tests + 32 integration tests, all passing.

### Spec — what's written

| Section | Status |
|---|---|
| §1 Overview | Complete, with scope (protocol + Native format) and "Native format is the only wire format over TCP" explanation |
| §2 Wire Format Primitives | Complete (VarUInt, fixed ints, String, Bool) |
| §3 Security | Complete (TLS, auth, inter-server secret) |
| §4 Protocol Versioning & Feature Gates | Complete, feature table current |
| §5 Packet Envelope | Complete |
| §6 Connection Lifecycle | Complete with state descriptions and phase-by-phase explanations |
| §7 Message Reference | Complete for all implemented messages |
| §8.1 Fixed-width types | Complete for implemented types + listed not-yet-implemented |
| §8.2 Variable-length types | Complete (String, FixedString) |
| §8.3 Composite types (fixed shape) | Placeholder only |
| §8.4 Versioned/stateful types | Concept explained, version table complete, sub-types not yet specified |
| §8.5 Types not yet categorized | Complete |
| §9 Compression | Placeholder only |
| §10 Packet Type Reference | Complete |
| §11 Implementation Notes | 15 entries covering real debugging wins |
| §12 Configuration | Complete (TCP + app-level) |

### Known problems with DESIGN.md's old assumptions (now fixed)

- **~~Stage A: v54460, Stage B: v54483~~** — The two-stage split was ambitious but the "current" target kept moving. Replaced with a single current-target plan (54483) and "tiers" of type support.
- **~~Problem 23: "54475: JSON column type support"~~** — **Wrong.** Protocol version 54475 is `QUERY_AND_LINE_NUMBERS` (script_query_number / script_line_number in ClientInfo). JSON support is gated by serialization-version prefixes inside the column data (§8.4.2 in the spec), not by a protocol-version feature. See spec §11.x and DESIGN.md Problem 23 below.
- **~~Feature name mismatch~~** — ch-go has `FeatureJSONStrings = 54475` which is a naming error that carried into the original DESIGN.md. Do not add a feature at 54475 named after JSON.

---

## Pending Work

Problems are sequenced so each one can be picked up independently. Each has a clear "done" criterion.

### Phase 6: Composite types (fixed shape) — §8.3 of the spec

Types in this group have a stable unversioned wire format. Sub-stream layouts are known statically from the type string.

#### Problem 25: `Nullable(T)`

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

#### Problem 26: `Array(T)`

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

#### Problem 27: `Tuple(T1, T2, ...)`

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

#### Problem 28: `Map(K, V)`

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

#### Problem 29: `Nested(...)`

Syntactic sugar — `Nested(a T1, b T2)` is equivalent to a pair of `Array(T1)` and `Array(T2)` columns, not a single composite. On the wire it's multiple top-level columns. Verify that the server flattens `Nested` before transmission (usually yes).

**Tests:** integration only, confirming that `Nested` columns arrive as parallel Array columns.

**Spec work:** §8.3 `Nested` subsection documenting the flattening behavior.

**References:**
- ch-go: uses `Array` + naming convention (no dedicated `col_nested.go`)
- ClickHouse: `src/DataTypes/DataTypeNested.h`, `DataTypeNested.cpp` (registration); `src/DataTypes/NestedUtils.h` (flattening logic)

---

### Phase 7: More fixed-width and parameterized types — §8.1

These are fixed-width but with parameter parsing or special encoding.

#### Problem 30: Remaining integer types

- `Int16` (2 bytes LE signed) — add to ColumnData and match list.
- `Float32` (4 bytes, IEEE 754 LE).
- `Float64` (8 bytes, IEEE 754 LE).
- `Bool` (1 byte, 0/1, domain over UInt8).

**Spec work:** move these from §8.1.4 "not yet implemented" to the main §8.1 table.

**References:**
- ch-go: `proto/col_int16_gen.go`, `proto/col_float32_gen.go`, `proto/col_float64_gen.go`, `proto/col_bool.go`
- ClickHouse: `src/DataTypes/DataTypeNumberBase.cpp`, `src/DataTypes/DataTypesNumber.cpp`, `src/DataTypes/Serializations/SerializationNumber.cpp`, `src/DataTypes/Serializations/SerializationBool.cpp`, `src/DataTypes/DataTypeDomainBool.cpp`

---

#### Problem 31: `Date`, `Date32`, `DateTime64`

- `Date` — 2 bytes, UInt16 days since `1970-01-01`.
- `Date32` — 4 bytes, Int32 days since `1970-01-01` (allows pre-1970).
- `DateTime64(scale)` or `DateTime64(scale, 'UTC')` — 8 bytes, Int64 ticks at the given scale.

**Implementation:**
- Parse scale from type string for `DateTime64`.
- Return decoded date/time as a structured value if a library is available; otherwise raw.

**Spec work:** subsections in §8.1 with byte-level examples.

**References:**
- ch-go: `proto/col_date_gen.go`, `proto/col_date32_gen.go`, `proto/col_datetime64.go`
- ClickHouse: `src/DataTypes/DataTypeDate.cpp`, `src/DataTypes/DataTypeDate32.cpp`, `src/DataTypes/DataTypeDateTime64.cpp`, `src/DataTypes/Serializations/SerializationDate.cpp`, `SerializationDate32.cpp`, `SerializationDateTime64.cpp`

---

#### Problem 32: `UUID`

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

#### Problem 33: `IPv4`, `IPv6`

- `IPv4` — 4 bytes.
- `IPv6` — 16 bytes (FixedString(16)-compatible).

**Spec work:** §8.1 subsections with examples.

**References:**
- ch-go: `proto/col_ipv4.go`, `proto/col_ipv6.go`
- ClickHouse: `src/DataTypes/DataTypeIPv4andIPv6.cpp`

---

#### Problem 34: `Enum16`

Wire-compatible with `Int16` (2 bytes LE signed). Same principle as `Enum8` (§11.8 in spec): variant labels live in the type string, byte layout is Int16.

**Tests:**
- Unit: encode/decode a small Enum16.
- Integration: `SELECT CAST(1 AS Enum16('a' = 1, 'b' = 30000))`.

**References:**
- ch-go: `proto/col_enum.go`
- ClickHouse: `src/DataTypes/DataTypeEnum.cpp`, `src/DataTypes/Serializations/SerializationEnum.cpp`

---

#### Problem 35: `Decimal(P, S)` and `Decimal32/64/128/256`

**Wire format:** 4/8/16/32 bytes LE signed integer representing `value * 10^S` where `S` is the scale.

**Implementation:**
- Parse `(P, S)` from type string.
- Store raw integer + scale; let caller interpret.

**Spec work:** §8.1 `Decimal` subsection.

**References:**
- ch-go: `proto/col_decimal32_gen.go`, `proto/col_decimal64_gen.go`, `proto/col_decimal128_gen.go`, `proto/col_decimal256_gen.go`
- ClickHouse: `src/DataTypes/DataTypesDecimal.cpp`, `src/DataTypes/Serializations/SerializationDecimal.cpp`, `SerializationDecimalBase.cpp`

---

#### Problem 36: `Int128`, `UInt128`, `Int256`, `UInt256`

Straight-up 16 or 32 byte little-endian two's-complement integers.

**Spec work:** §8.1 subsection.

**References:**
- ch-go: `proto/col_int128_gen.go`, `proto/col_uint128_gen.go`, `proto/col_int256_gen.go`, `proto/col_uint256_gen.go`
- ClickHouse: `src/DataTypes/DataTypesNumber.cpp`, `src/DataTypes/Serializations/SerializationNumber.cpp`

---

### Phase 8: Versioned/stateful types — §8.4

Implementation effort jumps significantly here. Each of these types has a serialization-version prefix and may maintain cross-block state.

#### Problem 37: `LowCardinality(T)` — simplest of the versioned types

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

#### Problem 38: `Variant(T1, T2, ...)`

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

#### Problem 39: `Dynamic`

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

#### Problem 40: `JSON` (Tier 1: String fallback only)

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

#### Problem 41: `JSON` (Tier 2: FLATTENED mode)

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

### Phase 9: Compression — §9 of spec

#### Problem 42: LZ4 compression

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

#### Problem 43: ZSTD compression

Same frame format, method byte `0x90`. Uses `zstd` crate. Spec work already covered by Problem 42.

**References:**
- ch-go: `compress/reader.go` (ZSTD branch)
- ClickHouse: `src/Compression/CompressionCodecZSTD.cpp`

---

### Phase 10: INSERT path — §6 (new state machine)

#### Problem 44: INSERT with a single block

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

#### Problem 45: Streaming INSERT (multiple blocks)

Extension of Problem 44. Let the caller push blocks incrementally.

**References:**
- ch-go: same as Problem 44
- clickhouse-go: `lib/driver/stdlib/stmt.go` (streaming batch pattern)

---

### Phase 11: Bring spec up to server v54483 — new feature sections

Document the features added between v54460 (current spec's cap) and v54483. Each feature goes into the §4.3 feature table; most also add fields to existing message types.

#### Problem 46: v54461 — password complexity rules

ServerHello gets: `VarUInt rules_count` + `count × (String pattern, String message)` if client ≥ 54461.

**Spec work:** update §7.2 ServerHello table + §4.3 feature table.

**References:**
- ClickHouse: `src/Server/TCPHandler.cpp` (~line 2205, `sendHello`); feature flag `DBMS_MIN_PROTOCOL_VERSION_WITH_PASSWORD_COMPLEXITY_RULES = 54461` in `src/Core/ProtocolDefines.h`
- ch-go: not implemented (v54460 cap)

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

### Phase 12: Polish and presentation

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

### Phase 13: Spec completion

#### Problem 70: Fill §8.3 Composite types

Turn the placeholder sketches in §8.3 into full specifications, byte-level examples included. Dependent on Problems 25–29.

---

#### Problem 71: Fill §8.4 LowCardinality, Variant, Dynamic, JSON

Same, dependent on Problems 37–41.

---

#### Problem 72: Replace §9 Compression placeholder

Full spec for LZ4/ZSTD framing, CityHash128, the activation setting. Dependent on Problem 42.

---

#### Problem 73: Add chunked-protocol section

Post-Problem 53. Major structural addition to §5 or a new §13.

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
| Connection + Query | `src/client.rs` | `src/Client/Connection.cpp`, `src/Server/TCPHandler.cpp` |
| Wire primitives | `src/proto/wire.rs` | `src/IO/VarInt.h`, `src/IO/WriteHelpers.h`, `src/IO/ReadHelpers.h` |
| Feature constants | `src/proto/feature.rs` | `src/Core/ProtocolDefines.h` |
| Packet enums | `src/proto/packet.rs` | `src/Core/Protocol.h` |
| Block/Column | `src/proto/block.rs`, `src/proto/column.rs` | `src/Formats/NativeWriter.cpp`, `src/Formats/NativeReader.cpp`, `src/DataTypes/` |
| Query options | `src/options.rs` | `src/Core/Settings.cpp` |
| Spec | `SPEC.md` | — |

---

## Scope boundaries (explicit non-goals)

- **HTTP interface** — out of scope. TCP native only.
- **Server-side implementation** — out of scope. Client only, with server behavior reverse-engineered for spec.
- **JSON Tier 3 (V3 on-disk format)** — not targeted; see §8.4.2.1 in spec.
- **Multi-threaded / async I/O** — out of scope. Single-threaded blocking I/O; `tokio` version is a separate future project.
- **TLS** — not implemented yet but noted in §3.1 as a transport-layer concern. Planned for post-demo work.
- **Inter-server mode** — out of scope. Client-to-server only.
