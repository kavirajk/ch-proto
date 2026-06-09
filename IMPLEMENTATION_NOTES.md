# Implementation Notes — ClickHouse Native Protocol & Native Format

These notes document the gotchas, footguns, and non-obvious behaviors encountered while implementing the ClickHouse native protocol and native format. Each entry describes a symptom (what a buggy implementation produces), the cause (the underlying protocol fact), and the fix (the correct pattern).

This document is a companion to `NATIVE_PROTOCOL.md` and `NATIVE_FORMAT.md`. It is **not normative** — every fact stated here is also documented in one of the spec docs. The value is in the format: postmortem-shaped, organized by failure mode, optimized for someone debugging an actual misbehavior.

The reference implementation is the Rust client in this repository. Where a note refers to specific code, file paths or library choices appear here — they do not appear in the spec docs.

---

## Table of Contents

1. [Protocol-level notes](#protocol-level-notes)
   - 1.1 [`ClientInfo.initial_address` must be non-empty in `host:port` format](#11-clientinfoinitial_address-must-be-non-empty-in-hostport-format)
   - 1.2 [`ClientInfo.initial_time` is Int64, not VarUInt](#12-clientinfoinitial_time-is-int64-not-varuint)
   - 1.3 [`BlockInfo.bucket_number` default is `-1`, not `0`](#13-blockinfobucket_number-default-is--1-not-0)
   - 1.4 [Data packets are symmetric — both directions carry `table_name`](#14-data-packets-are-symmetric--both-directions-carry-table_name)
   - 1.5 [Packet type codes are VarUInt, not UInt8](#15-packet-type-codes-are-varuint-not-uint8)
   - 1.6 [First Data packet after a query is the header block (0 rows)](#16-first-data-packet-after-a-query-is-the-header-block-0-rows)
   - 1.7 [Log and ProfileEvents must be decoded even when ignored](#17-log-and-profileevents-must-be-decoded-even-when-ignored)
   - 1.8 [Multiple Progress packets are cumulative, not deltas](#18-multiple-progress-packets-are-cumulative-not-deltas)
   - 1.9 [Query parameter values must be single-quoted on the wire](#19-query-parameter-values-must-be-single-quoted-on-the-wire)
   - 1.10 [Client must declare a protocol version at or above each feature it needs](#110-client-must-declare-a-protocol-version-at-or-above-each-feature-it-needs)
   - 1.11 [ServerHello `password_complexity_rules` needs a bounded decode](#111-serverhello-password_complexity_rules-needs-a-bounded-decode)
2. [Format-level notes](#format-level-notes)
   - 2.1 [Column must include `has_custom_serialization` byte at v54454+](#21-column-must-include-has_custom_serialization-byte-at-v54454)
   - 2.2 [`Enum8` / `Enum16` are wire-compatible with `Int8` / `Int16`](#22-enum8--enum16-are-wire-compatible-with-int8--int16)
   - 2.3 [Column type strings carry parameters — strip before matching](#23-column-type-strings-carry-parameters--strip-before-matching)
   - 2.4 [Unknown column types are a hard decode failure](#24-unknown-column-types-are-a-hard-decode-failure)
   - 2.5 [ProfileEvents `value` column type varies between blocks](#25-profileevents-value-column-type-varies-between-blocks)
   - 2.6 [Tuple parsing requires depth-aware comma splitting; row count comes from inner elements](#26-tuple-parsing-requires-depth-aware-comma-splitting-row-count-comes-from-inner-elements)
   - 2.7 [UUID is transmitted as two byte-swapped LE UInt64 halves](#27-uuid-is-transmitted-as-two-byte-swapped-le-uint64-halves)
   - 2.8 [Versioned-type state prefix is per column per query — header blocks emit nothing](#28-versioned-type-state-prefix-is-per-column-per-query--header-blocks-emit-nothing)
3. [Reference Rust client status](#reference-rust-client-status)

---

## Protocol-level notes

### 1.1 `ClientInfo.initial_address` must be non-empty in `host:port` format

**Symptom.** Server rejects Query with an assertion violation in `SocketAddress::init()` complaining that `hostAndPort` is empty.

**Cause.** The server parses `initial_address` via a socket address parser that fails if the string is empty.

**Fix.** Always send a valid `host:port` string, e.g., `"127.0.0.1:0"`. Port `0` is fine — the server uses this only for logging, not for actual connections.

---

### 1.2 `ClientInfo.initial_time` is Int64, not VarUInt

**Symptom.** Server rejects Query with `CANNOT_READ_ALL_DATA` — reporting a byte count shortfall (e.g., "Bytes read: 54. Bytes expected: 108.").

**Cause.** `initial_time` is a fixed-width Int64 (8 bytes, little-endian) on the wire. Encoding it as VarUInt under-runs the server's expected byte count by up to 7 bytes (VarUInt encodes `0` in 1 byte vs. 8 bytes for Int64).

**Fix.** Use a fixed-width 8-byte little-endian write for `initial_time`. Do not confuse this with other numeric fields in ClientInfo (`version_major`, `version_minor`, `protocol_version`, `distributed_depth`, etc.) which are VarUInt.

**General rule.** Within ClientInfo specifically, timestamps are fixed-width; everything else numeric is VarUInt. Always consult the field tables in `NATIVE_PROTOCOL.md` §6 (ClientInfo) for the authoritative type of each field.

---

### 1.3 `BlockInfo.bucket_number` default is `-1`, not `0`

**Symptom.** Server misinterprets normal result blocks as belonging to aggregation bucket `0`, leading to incorrect distributed query behavior.

**Cause.** `0` is a valid bucket number (first bucket in a two-level GROUP BY aggregation). The "no bucket" sentinel is `-1`.

**Fix.** Default-construct BlockInfo with `bucket_number = -1`. Only set it to a non-negative value when actually emitting bucketed aggregation blocks (inter-server use only; external clients should always send `-1`).

---

### 1.4 Data packets are symmetric — both directions carry `table_name`

**Symptom.** Client hangs on read after query, or decodes garbage for column names/types.

**Cause.** The Data packet wire format is symmetric — both directions include an empty `String` (table name) before the Block. Failing to read the `table_name` before decoding the Block on the server → client path misaligns every subsequent field.

**Fix.** When reading a server-side Data packet, read a `String` first (the table name, almost always empty for query results) before reading the Block body. See `NATIVE_FORMAT.md` §3 for the full Block wire format.

---

### 1.5 Packet type codes are VarUInt, not UInt8

**Symptom.** Works in testing (all current packet type codes are < 128, where VarUInt and UInt8 produce identical bytes), but future packet types ≥ 128 would silently break compatibility.

**Cause.** The protocol's formal encoding for packet type codes is VarUInt, not fixed-width. Current implementations happen to work with UInt8 only because all packet type codes are small (0–18).

**Fix.** Always use VarUInt encoding for packet type codes on both encode and decode paths. See `NATIVE_PROTOCOL.md` §4.

---

### 1.6 First Data packet after a query is the header block (0 rows)

**Symptom.** `SELECT 1` returns 1 column and **0 rows** instead of 1 row with value `1`.

**Cause.** The server's response to a query is a stream of packets, not a single packet:

```
Data (header:  N cols, 0 rows)     ← schema announcement, no data
Data (result:  N cols, M rows)     ← actual data (0 or more such blocks)
...
Data (empty:   0 cols, 0 rows)     ← boundary marker, still NOT the end
EndOfStream                         ← authoritative end of query
```

A client that reads only one Data packet gets the header block — which correctly announces the columns but has zero rows. The actual data arrives in subsequent Data packets.

`num_rows = 0` does **not** mean end-of-query. Only `EndOfStream` (packet type 5) signals the end of a query response.

**Fix.** After sending the Query + end-of-client-data marker, loop reading packets until `EndOfStream` or `Exception`. Treat the first Data packet as the schema; accumulate rows from subsequent Data packets. See `NATIVE_PROTOCOL.md` §5.3 for the full dispatch table.

---

### 1.7 Log and ProfileEvents must be decoded even when ignored

**Symptom.** Connection hangs or produces garbage after a query that emitted Log or ProfileEvents packets.

**Cause.** The packet envelope does not include a body length. A client that reads the packet type byte and then attempts to skip to "the next packet" will consume bytes from the middle of the current packet's payload.

**Fix.** Always fully decode the bodies of Log and ProfileEvents packets, even when the client intends to discard the values. The stream position must advance by exactly the body length, and the only way to compute that length is to parse the block structure.

The same reasoning applies to `Totals`, `Extremes`, `TableColumns`, `Progress`, and `ProfileInfo` — a client may ignore the semantic content but must always consume the bytes.

---

### 1.8 Multiple Progress packets are cumulative, not deltas

**Symptom.** Client-side row counts from Progress packets appear inflated (2×, 3×, … the actual server count).

**Cause.** Each Progress packet carries cumulative totals since the start of the query, not deltas from the previous Progress packet. Summing consecutive Progress packets double-counts.

**Fix.** Treat each Progress packet as a snapshot of the query's running totals. Replace the previous value rather than add to it. The last Progress packet received before `EndOfStream` contains the final totals for the query.

---

### 1.9 Query parameter values must be single-quoted on the wire

**Symptom.** A query like `SELECT {x:UInt32}` with a parameter `x = 42` fails with:

```
DB::Exception: Substitution `x` is not set
```

even though the client sent a parameter named `x`.

**Cause.** Query parameters are transported as custom settings in the Query packet's settings list, with the `Custom` flag (`0x02`) set. When the server converts those settings into the query parameter map, it unwraps each value using single-quote-delimited string parsing. A bare value (e.g., `42`) fails this unwrap and the parameter is dropped silently — the server then reports "Substitution is not set" for the named parameter at query-execution time.

**Fix.** Wrap the parameter value in single quotes on encode, and unwrap them on decode. Inner single quotes must be escaped by doubling (`'` → `''`). Examples:

| Logical value | Wire value     |
|---------------|----------------|
| `42`          | `'42'`         |
| `hello`       | `'hello'`      |
| `it's`        | `'it''s'`      |
| empty string  | `''`           |

This quoting is internal to the parameter transport — the query SQL and parameter names are not affected. Only the parameter value string needs this treatment.

---

### 1.10 Client must declare a protocol version at or above each feature it needs

**Symptom.** A client feature works against unit tests and against some server versions but silently fails with older-looking behavior — e.g., query parameters appear to be sent but the server doesn't find them; the request succeeds minus the parameter-dependent feature.

**Cause.** The negotiated protocol version is `min(client_declared, server_declared)`. Every feature is gated by a minimum version. A client that declares a max version below the feature's gate will not emit that feature on the wire — even if the server supports it.

For example, declaring the client's max version as the `ADDENDUM` feature version (54458) means the `PARAMETERS` feature (54459) is never active — parameters are silently omitted from the Query packet body because the feature check fails at encode time.

**Fix.** The client's declared `protocol_version` in ClientHello must be at least the maximum version of any feature the client wants to use. In practice, declare the highest version supported by the implementation and let version negotiation pick the actual working version.

This is a silent failure mode: no error is emitted during encoding, and the server often accepts the malformed packet and simply executes the query without the expected feature data. Hard to debug without diffing against known-good packet captures.

---

### 1.11 ServerHello `password_complexity_rules` needs a bounded decode

**Symptom.** A connection succeeds against a normally-configured server but a hostile or misconfigured server can force the client into an arbitrarily large allocation during handshake — at worst, an OOM before the first query runs.

**Cause.** At negotiated version ≥ 54461, ServerHello carries `VarUInt count` followed by `count × (String pattern, String message)`. The wire encoding of `count` is one to ten bytes, but the decoded value can reach `2^64 − 1`. A decoder that treats `count` as trusted and pre-allocates `Vec::with_capacity(count)` (or the language equivalent) hands the server a memory-amplification primitive — a single ten-byte VarUInt can request a multi-exabyte allocation.

The same hazard applies, to a smaller degree, to the inner `pattern` and `message` strings: the standard length-prefixed `String` reader allocates up to the declared length before reading any payload bytes.

**Fix.** Treat both the rule count and the per-string length as untrusted. Cap and reject:

- `count > 256` → protocol error, tear down the connection. (The canonical C++ server enforces the same cap at send time; the canonical C++ client enforces it on receive.)
- `pattern.len() > 4096` or `message.len() > 4096` → protocol error.

Both caps match the C++ constants `DBMS_MAX_PASSWORD_COMPLEXITY_RULES` and `DBMS_MAX_HELLO_STRING_SIZE` in `src/Core/ProtocolDefines.h`.

A useful generalisation: **never `reserve()` based on an untrusted length without an upstream sanity bound**. This applies to every server-supplied count in the handshake, and to most server-supplied counts in the rest of the protocol — though most other counts (row counts, column counts, etc.) sit downstream of large packet bodies that the client must already have buffered, which puts an implicit bound on the value the server can choose.

**Display-layer note.** The strings originate from operator config (`<password_complexity>` blocks). If the client surfaces them to users, run them through control-character sanitization at the display boundary — operators can paste in newlines or terminal escape sequences that would otherwise garble a terminal. This is a UI concern, not a protocol concern, and lives outside the wire decoder.

---

## Format-level notes

### 2.1 Column must include `has_custom_serialization` byte at v54454+

**Symptom.** After decoding the first result block, the next read reads what looks like a `Hello` packet (packet type `0`) — but the handshake is long over. The stream is misaligned by exactly one `0x00` byte per column.

A variant: INSERT data sent with columns is rejected or misparsed by the server, because every column is missing one byte.

**Cause.** At negotiated protocol ≥ 54454 (feature `CUSTOM_SERIALIZATION`), every Column carries a `UInt8` byte after the type string, indicating whether the column uses a non-default serialization (sparse, low-cardinality, etc.). For standard columns, this byte is `0`.

Clients that skip this byte read the server's next-packet-type-code out of the middle of the previous packet. Since the byte is `0x00`, it appears to be a `Hello` packet (server packet type 0), but the rest of the stream is garbage.

This pitfall is easy to miss during testing if:
- The client only sends empty Data packets (num_columns = 0), so the Column encode path is never exercised.
- The client only handles the header Data packet from the server (which has columns but 0 rows of data, so the misalignment doesn't surface until the next packet is read).

**Fix.** In both Column encode and Column decode, gate reading/writing the `has_custom_serialization` byte on the `CUSTOM_SERIALIZATION` feature:

- **Encode.** Write `0` for standard columns. To represent a non-default serialization, model it explicitly (e.g., a `Serialization` enum with `Default` / `Custom { kind_stack }` variants) and write `1` followed by the kind_stack.
- **Decode.** Read the byte. If `0`, continue. If `1`, either decode the kind_stack or return an `Unsupported` error — whichever matches the client's capability.

Pass the negotiated protocol version through the Block encode and decode functions so Column methods can check the feature gate.

---

### 2.2 `Enum8` / `Enum16` are wire-compatible with `Int8` / `Int16`

**Symptom.** Decoding ProfileEvents (or other blocks with Enum columns) fails with "unsupported column type" — even though the spec describes the column as `Int8`.

**Cause.** The server sends types like `Enum8('increment' = 1, 'gauge' = 2)` for columns the spec describes as `Int8` (e.g., the ProfileEvents `type` column). The wire bytes are identical to `Int8` — one byte per row — but the type string on the wire differs.

**Fix.** Treat `Enum8` as `Int8` and `Enum16` as `Int16` during column decoding. The preferred approach is to strip the `(...)` parameter suffix from the type string and dispatch on the base name (see §2.3 below).

---

### 2.3 Column type strings carry parameters — strip before matching

**Symptom.** Decoding a column with type `DateTime('UTC')`, `FixedString(16)`, `Decimal(9, 2)`, `Nullable(UInt32)`, or `Array(Int32)` fails with "unsupported column type" — even when the base type is supported by the client.

**Cause.** Type names on the wire include parameters in parentheses. A decoder that dispatches on the exact type string will miss parameterized variants of supported types. This is pervasive: `DateTime` always carries a timezone, `Decimal` carries precision and scale, and `Enum` / `Nullable` / `Array` / `Tuple` / `Map` all wrap a subtype.

**Fix.** When dispatching on the type string, extract the base type by taking the substring before the first `(`. Example: `"DateTime('UTC')"` and `"DateTime(3)"` both reduce to the base type `"DateTime"`.

The parameter content inside the parentheses may still be needed for decoding (e.g., `Decimal(P, S)` scale affects value interpretation, `FixedString(N)` determines row size, `Nullable(T)` affects wire layout). So don't discard the parameters permanently — just use only the base name for the type dispatch.

---

### 2.4 Unknown column types are a hard decode failure

**Symptom.** Decoding a Data or Log block fails and leaves the stream in an inconsistent state; subsequent packet reads produce garbage.

**Cause.** Unlike fixed-layout packets (Progress, ProfileInfo) where fields have known sizes, column data sizes depend on the type: `UInt32` = 4 bytes per row, `String` = variable (length-prefixed per value), `Array(T)` = offsets + nested element data. Without knowing the type, the decoder cannot compute the byte span of that column to skip over it.

**Fix.** On encountering an unknown column type, the decoder must fail the entire query and terminate or reset the connection. There is no "skip this column" fallback — the stream is permanently misaligned. This motivates supporting at least the common types (UInt and Int variants, String, DateTime, Nullable) before targeting production workloads.

Note the asymmetry with "ignored but still decoded" packets (Log, ProfileEvents): a client may choose to discard the packet's decoded content after the fact, but the bytes must still be consumed, and consuming those bytes requires understanding every column type in the block.

---

### 2.5 ProfileEvents `value` column type varies between blocks

**Symptom.** Decoding a ProfileEvents block fails because column 6 (`value`) is declared as `Int64` in one packet and `UInt64` in another.

**Cause.** The `value` column in ProfileEvents is **not** a single fixed type. Each ProfileEvents packet declares its own wire type for the column based on the events it carries: always-increasing counters (e.g., `Query`, `NetworkReceiveBytes`) use `UInt64`, while gauges and delta metrics use `Int64`. The declared column type is uniform within a single packet but may differ between packets during one query's response stream.

**Fix.** Decode the column according to the wire type declared in each packet, not based on an assumed fixed type. Clients that want a unified representation can widen to a signed 64-bit integer, accepting that unsigned values at or above 2^63 either need explicit handling or are treated as a decode error.

A simpler alternative is to store the `value` column as raw bytes plus the type string, deferring interpretation to the caller.

---

### 2.6 Tuple parsing requires depth-aware comma splitting; row count comes from inner elements

**Symptom — type parsing.** A `Tuple(Tuple(Int8, Int32), String)` decode fails with a cryptic "unknown type" error, or a `Tuple(Map(String, UInt32))` blows up at the inner Map decode. The decoder believes the element types are fragments like `"Tuple(Int8"` or `"Map(String"` because the type string was split on every comma.

**Cause.** Unlike `Nullable(T)` and `Array(T)`, which have a single inner type that can be extracted with `find('(')` / `rfind(')')`, `Tuple(...)` carries *N* element types separated by `,`. A naive `inner.split(',')` does not know that some commas live inside nested parentheses (other Tuples, Maps, parameterised DateTime, etc.) and splits in the wrong places.

**Fix.** Split with a depth counter. Walk the inner string char by char; track depth (`+1` on `(`, `-1` on `)`); only split when depth `== 0`. Reject the type string if depth doesn't end at `0` (unbalanced parens):

```
function split_with_composite(s):
    depth = 0
    out = []
    start = 0
    for (i, c) in enumerate(s):
        if c == '(':       depth += 1
        elif c == ')':     depth -= 1
        elif c == ',' and depth == 0:
            out.push(trim(s[start..i]))
            start = i + 1
    if depth != 0: error("unbalanced parens")
    out.push(trim(s[start..]))
    return out
```

The same pattern applies the moment any other multi-arg type (`Map(K, V)`, `Variant(T1, T2, ...)`, named tuple element lists) lands in the decoder.

**Symptom — row counting.** Validation passes when it shouldn't, encodes produce malformed streams, or `Array(Tuple(...))` fails at the outer Array's invariant check (`inner.row_count() != offsets.last()`) for a tuple that's actually consistent.

**Cause.** A Tuple's in-memory representation is most naturally modeled as a vector of *N* element columns. The temptation is to make `row_count()` return `vec.len()`. But `vec.len()` is the **arity** (number of element types), not the row count. Element columns are parallel; the row count is the row count of any one of them.

**Fix.** Implement `row_count()` for Tuple as "the row count of the first element column, or 0 if empty." Validate at encode time that all elements agree on row count (and recurse into each to catch nested invariants).

**Symptom — decoding inner streams.** A multi-row Tuple decodes only the first row's worth of data per element, then either errors out reading past the buffer or produces nonsense for subsequent rows.

**Cause.** Calling the inner decode function with `1` as the row count for each element treats the wire as one value per element rather than `num_rows` values. Tuple's wire format is per-element streams of `num_rows` values each; the outer `num_rows` must be passed through unchanged.

**Fix.** Pass the outer `num_rows` to each element's decode call.

---

### 2.7 UUID is transmitted as two byte-swapped LE UInt64 halves

**Symptom.** A UUID column round-trips with garbled bytes — the canonical UUID `550e8400-e29b-41d4-a716-446655440000` decodes as `d44119be-2008-4055-0000-44556644a716` (or a similarly-shuffled value), and SQL queries that filter by UUID literal don't match.

**Cause.** ClickHouse's wire format for `UUID` is not the canonical 16 big-endian bytes you might expect. Each 8-byte half is byte-reversed independently (i.e., each half is written little-endian as if it were a `UInt64`). This is a historical quirk tied to ClickHouse internals (issue #34369) and is not something the protocol can change without breaking compatibility.

To convert canonical → wire (or wire → canonical, since the operation is its own inverse):

1. Take the 16 canonical bytes.
2. Reverse bytes `0..7` in place.
3. Reverse bytes `8..15` in place.
4. Write the resulting 16 bytes verbatim.

**Worked example** — UUID `550e8400-e29b-41d4-a716-446655440000`:

| Step                        | Bytes                                              |
|-----------------------------|----------------------------------------------------|
| Canonical (big-endian)      | `55 0E 84 00 E2 9B 41 D4 A7 16 44 66 55 44 00 00` |
| Reverse first half          | `D4 41 9B E2 00 84 0E 55 A7 16 44 66 55 44 00 00` |
| Reverse second half (wire)  | `D4 41 9B E2 00 84 0E 55 00 00 44 55 66 44 16 A7` |

**Fix.** Apply the byte-swap at the encode/decode boundary, not in the in-memory representation. Most client libraries expose canonical UUID values to users and confine the swap to a single helper called from the column encoder/decoder.

A natural lurking bug: a "reverse the whole 16 bytes" implementation is wrong (it puts byte 0 where byte 15 belongs, scrambling both halves together). The two halves must be reversed independently.

---

### 2.8 Versioned-type state prefix is per column per query — header blocks emit nothing

**Symptom.** Decoding a `LowCardinality(...)` or `JSON` column from a normal SELECT response consumes too many bytes and the next read picks up garbage interpretable as the start of a fresh Data packet (e.g., `01 00 02 ff ff ff ff 00 ...` which is BlockInfo for a new block, or a column name like `\x17 'CAST(...'`). The decoder's state-prefix read appears to succeed but with nonsense values like `Int64 = -1090921627647`.

**Cause.** Versioned types (`LowCardinality`, `Variant`, `Dynamic`, `JSON`, `Object`) carry their state prefix once per column per query, not per block. The server emits the prefix only before the first block whose row count is greater than zero. The header block (rows = 0) and any subsequent blocks contain only the per-block payload — no state prefix.

A naive decoder that reads the state prefix on every block double-counts it on the data block, consuming bytes that belong to the next packet's header.

**Wire-level evidence** (probe of `SELECT '{"a":1}'::JSON`):

- Header Data packet: empty block (rows = 0), no JSON column body.
- Data Data packet: starts with `01 00 00 00 00 00 00 00` (state prefix), then `09 '{...}'` (String with text).

If the decoder reads the state prefix on the header block, it consumes 8 bytes that don't exist — taking it 8 bytes into the next packet.

**Fix (single-block-aware).** Treat `rows == 0` as "no state prefix, no per-block data" — return an empty column immediately. This works for:

- The header block (always rows = 0).
- Any single-data-block query (the common case for ad-hoc SELECTs and `arrayJoin([...])`-style row generators).

**What this fix does NOT cover.** Queries returning multiple data blocks for the same versioned-type column (e.g., large SELECTs from a real table where the server batches results into multiple Data packets). The second+ data block has rows > 0 but no state prefix on the wire — a decoder using the simple fix would re-read the prefix from the per-block metadata bytes and fail.

**Fully correct implementation** requires tracking per-column state across blocks within a query, similar to clickhouse-go's `ReadStatePrefix` / `WriteStatePrefix` lifecycle on the column object. That's a larger refactor of the Block decoder's responsibilities — currently each Block decode constructs fresh Column objects with no memory of previous blocks in the same response.

This pitfall affects every type in `NATIVE_FORMAT.md` §4.4 (versioned types). The same `rows == 0 → skip` workaround applies identically to `LowCardinality` and `JSON` Tier 1.

---

## Reference Rust client status

This repository contains a Rust implementation of the client side of the protocol. The status of each spec area in that implementation is documented here.

### Implemented

- **Protocol:** Handshake (with addendum), Ping/Pong, Query, INSERT (single-block and streaming), response stream loop.
- **Messages:** Hello, Query, ClientInfo, Setting, Parameter, Block, Progress, ProfileInfo, Totals, Extremes, Log, ProfileEvents, TableColumns, Exception, EndOfStream.
- **Format primitives:** VarUInt, fixed-width integers (8/16/32/64/128/256 bit, both signed and unsigned), String, FixedString, Bool, Float32, Float64.
- **Date/time:** Date, Date32, DateTime, DateTime64.
- **Domain types:** UUID (with the byte-swap quirk handled at the boundary), IPv4, IPv6, Enum8, Enum16, Decimal(P, S) at all four widths.
- **Composites:** Nullable, Array, Tuple, Map, Nested.
- **Versioned types (Tier 1 only):** LowCardinality, JSON Tier 1 (String fallback). Both subject to the single-data-block limitation (§2.8).
- **Compression frame primitives:** LZ4, ZSTD, NONE encode/decode with CityHash102 verification. Connection-level integration is not yet wired up — `with_compression(true)` sets the wire flag but the response decoder cannot consume compressed blocks.

### Out of scope for this revision

- **`Variant(T1, T2, ...)`** — discriminated union with discriminator dispatch (BASIC and COMPACT modes). Empirical verification across server versions is incomplete.
- **`Dynamic`** — runtime-typed column built on Variant.
- **`JSON` Tier 2 (FLATTENED) and Tier 3 (V3)** — `Object`-rooted format layered over LowCardinality / Variant / Dynamic.
- **Multi-block versioned types.** See §2.8.
- **Compression connection integration.** Frame primitives exist; the connection-level stream wrapper that intercepts Block boundaries is not implemented.
- **Cancel packet** during query phase.
- **SSH challenge-response authentication** (v54466+).
- **Query plan serialization** (v54477+).
- **Chunked protocol** (v54470+).
- **AggregateFunction**, **SimpleAggregateFunction**, **Interval**, **Geo types** (Point, Ring, Polygon, MultiPolygon).

### Library and dependency choices

| Concern | Library |
|---------|---------|
| UUID in-memory representation | `uuid` crate |
| LZ4 compression | `lz4_flex` crate (pure Rust, block format, not the LZ4 frame format) |
| ZSTD compression | `zstd` crate |
| CityHash128 (compression-frame checksum) | `clickhouse-rs-cityhash-sys` crate. ClickHouse uses CityHash v1.0.2, NOT modern Google CityHash; the two produce different outputs. |
| 256-bit integers (Int256, UInt256) | Raw `[u8; 32]` LE two's-complement arrays. Rust has native `i128`/`u128` but no 256-bit integers. The `ch-tsv` TabSeparated formatter renders them directly via long division on the magnitude (`u256_to_decimal` in `src/tsv.rs`); other callers convert to a big-int type if needed. |
| 256-bit decimal (Decimal256) | Same — raw `[u8; 32]` plus scale metadata; `ch-tsv` renders with the shared sign/scale logic (`write_decimal256`). |

### Where to find each piece

| Concern | File |
|---------|------|
| Wire primitives | `src/proto/wire.rs` |
| Block / BlockInfo | `src/proto/block.rs` |
| Column / ColumnData / type encoding | `src/proto/column.rs` |
| Compression frame | `src/proto/compression.rs` |
| Connection lifecycle / handshake / query / INSERT | `src/client.rs` |
| Query options | `src/options.rs` |
| Examples | `examples/events.rs` (flat types), `examples/catalog.rs` (composites + versioned + Decimal) |
