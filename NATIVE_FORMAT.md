# ClickHouse Native Format

This specification describes the wire format used to transmit tabular data within ClickHouse. The Native format appears in several places:

- The body of Data, Totals, Extremes, Log, ProfileEvents, and TableColumns packets in the TCP native protocol (`NATIVE_PROTOCOL.md`).
- The output of `SELECT ... FORMAT Native` over HTTP.
- File exports written with `INTO OUTFILE ... FORMAT Native`.
- Inter-server replication payloads.

This document describes only the bytes inside a Block — the columnar payload — and the column-level type encodings that build it. Packet framing, connection state, and version negotiation are described in `NATIVE_PROTOCOL.md`.

All multi-byte integer fields are encoded little-endian. Signed integers use two's complement.

## Table of Contents

1. [Wire Primitives](#1-wire-primitives)
2. [Block & Column Structure](#2-block--column-structure)
3. [Data Types](#3-data-types)
   - 3.1 [Fixed-width Types](#31-fixed-width-types)
   - 3.2 [Variable-length Types](#32-variable-length-types)
   - 3.3 [Composite Types](#33-composite-types)
   - 3.4 [Versioned Types](#34-versioned-types)
4. [Compression Frame](#4-compression-frame)
5. [Glossary](#5-glossary)

---

## 1. Wire Primitives

The Native format builds on four primitive encodings.

| Primitive       | Size     | Description |
|-----------------|----------|-------------|
| VarUInt         | 1–10 B   | LEB-128 variable-length unsigned integer |
| Fixed-width int | 1, 2, 4, 8, 16, 32 B | Little-endian, two's complement for signed |
| String          | variable | VarUInt length prefix + raw bytes |
| Bool            | 1 B      | `0x00` = false, non-zero = true |

### 1.1 VarUInt

A variable-length unsigned integer using LEB-128 encoding. Each byte carries 7 data bits in positions 0–6 and 1 continuation bit in position 7. The continuation bit is `1` if more bytes follow and `0` on the final byte.

| Value range            | Bytes |
|------------------------|-------|
| 0 – 127                | 1     |
| 128 – 16383            | 2     |
| 16384 – 2097151        | 3     |
| up to full UInt64      | up to 10 |

**Encoding example.** Value `300`:

```
300 = 0b100101100

Byte 0: 0xAC = 0b10101100   (data: 0101100, continuation: 1)
Byte 1: 0x02 = 0b00000010   (data: 0000010, continuation: 0)
```

**Decoding example.** Bytes `0xAC 0x02`:

```
Byte 0: data = 0x2C, continuation = 1 → accumulator = 0x2C, shift = 7
Byte 1: data = 0x02, continuation = 0 → accumulator = (0x02 << 7) | 0x2C = 300
```

### 1.2 Fixed-width Integers

| Type   | Bytes | Encoding                                      |
|--------|-------|-----------------------------------------------|
| UInt8  | 1     | Raw byte                                      |
| UInt16 | 2     | Little-endian                                 |
| UInt32 | 4     | Little-endian                                 |
| UInt64 | 8     | Little-endian                                 |
| UInt128| 16    | Little-endian                                 |
| UInt256| 32    | Little-endian                                 |
| Int8   | 1     | Raw byte, two's complement                    |
| Int16  | 2     | Little-endian, two's complement               |
| Int32  | 4     | Little-endian, two's complement               |
| Int64  | 8     | Little-endian, two's complement               |
| Int128 | 16    | Little-endian, two's complement               |
| Int256 | 32    | Little-endian, two's complement               |
| Float32| 4     | IEEE 754 single-precision, little-endian      |
| Float64| 8     | IEEE 754 double-precision, little-endian      |

**Encoding example.** UInt32 value `1` → `01 00 00 00`. Int32 value `-1` → `FF FF FF FF`.

### 1.3 String

A length-prefixed byte sequence:

```
[VarUInt: byte_length] [byte_length bytes: raw value]
```

The byte sequence is not required to be valid UTF-8. Empty strings encode as a single `0x00` byte. Strings may contain any byte values including embedded NUL.

**Encoding example.** String `"ab"`:

```
02 61 62
```

**Decoding example.** Bytes `02 61 62`:

```
Read VarUInt → length = 2
Read 2 bytes → "ab"
```

### 1.4 Bool

A single byte. `0x00` is false; any non-zero value is true (canonically `0x01`).

---

## 2. Block & Column Structure

A **Block** is the unit of data exchange in the Native format — a self-describing chunk of rows organized columnar. All values for column 1 are stored together, then all values for column 2, and so on. A Block contains only the columns referenced by the query, not all columns of any underlying table.

### 2.1 Block wire layout

```
[BlockInfo]               metadata (when the BLOCK_INFO feature is active)
[VarUInt: num_columns]    number of columns in this block
[VarUInt: num_rows]       number of rows in this block
[Column × num_columns]    column entries, omitted when num_columns = 0
```

When the enclosing protocol predates `BLOCK_INFO` (version 51903), the BlockInfo prefix is omitted; everything else is identical.

### 2.2 BlockInfo

BlockInfo uses **field-tagged encoding** for forward compatibility. Each field is preceded by a VarUInt field ID. A field ID of `0` terminates the structure. Unknown field IDs are skipped (the decoder reads the value according to the type implied by the field ID).

| Field ID | Field         | Type  | Description                                    |
|----------|---------------|-------|------------------------------------------------|
| 1        | is_overflows  | UInt8 | Overflow block from GROUP BY. `0` for non-overflow blocks. |
| 2        | bucket_number | Int32 | Aggregation bucket. `-1` for non-bucketed blocks. |
| 0        | (terminator)  | —     | End of BlockInfo. Always required.             |

**Wire layout:**

```
[VarUInt: 1] [UInt8: is_overflows]
[VarUInt: 2] [Int32: bucket_number]
[VarUInt: 0]
```

### 2.3 Column wire layout

A Column appears `num_columns` times within a Block.

| # | Field                    | Type    | Condition                | Description |
|---|--------------------------|---------|--------------------------|-------------|
| 1 | name                     | String  | always                   | Column name |
| 2 | type                     | String  | always                   | ClickHouse type string (e.g., `"UInt64"`, `"Array(String)"`) |
| 3 | has_custom_serialization | UInt8   | feature `CUSTOM_SERIALIZATION` (v54454) | `0` = default, `1` = custom (kind_stack follows) |
| 4 | kind_stack               | bytes   | when field 3 = `1`       | Opaque metadata describing the non-default serialization (sparse, etc.). Not specified here. |
| 5 | data                     | bytes   | always                   | Column values for all `num_rows` rows. Layout per type — see §3. |

### 2.4 Block variants

All Data-family packets use the same Block wire format. Block variants are defined by the column and row counts:

| Variant       | num_columns | num_rows | Purpose |
|---------------|-------------|----------|---------|
| Header block  | N > 0       | 0        | Announces the result schema (column names + types). |
| Result block  | N > 0       | M > 0    | Actual result rows. |
| Empty block   | 0           | 0        | Sentinel — end-of-input on the client side; boundary marker on the server side. |

All three are the same wire structure with different row/column counts.

### 2.5 Byte-level examples

#### Empty block (with BlockInfo)

```
01 00                   BlockInfo: field_id=1, is_overflows=0
02 FF FF FF FF          BlockInfo: field_id=2, bucket_number=-1
00                      BlockInfo terminator
00                      num_columns = 0
00                      num_rows = 0
```

Total: 8 bytes.

#### Header block for `SELECT 1`

The block announces one column named `"1"` of type `UInt8`, with zero rows. At protocol ≥ 54454, the `has_custom_serialization` byte is included.

```
01 00                   BlockInfo: is_overflows = 0
02 FF FF FF FF          BlockInfo: bucket_number = -1
00                      BlockInfo terminator
01                      num_columns = 1
00                      num_rows = 0
01 "1"                  Column[0].name = "1"
05 "UInt8"              Column[0].type = "UInt8"
00                      Column[0].has_custom_serialization = 0
                        Column[0].data: no bytes (num_rows = 0)
```

#### Result block for `SELECT 1` (one row)

```
01 00                   BlockInfo: is_overflows = 0
02 FF FF FF FF          BlockInfo: bucket_number = -1
00                      BlockInfo terminator
01                      num_columns = 1
01                      num_rows = 1
01 "1"                  Column[0].name = "1"
05 "UInt8"              Column[0].type = "UInt8"
00                      Column[0].has_custom_serialization = 0
01                      Column[0].data: one UInt8 byte = 1
```

---

## 3. Data Types

This section documents every type the Native format can encode within a Column's `data` field. Types are organised into four families in increasing decoder complexity:

| Family                           | Section | Cardinality      | State?    |
|----------------------------------|---------|------------------|-----------|
| Fixed-width                      | §3.1    | One stream       | None      |
| Variable-length                  | §3.2    | One stream       | None      |
| Composite (fixed shape)          | §3.3    | Multiple streams | None      |
| Versioned / stateful             | §3.4    | Multiple streams | Per-column state prefix; possibly cross-block state |

A decoder dispatches on the column's `type` string (the second field of the Column header — see §2.3). Type strings often carry parameters in parentheses; the decoder strips the `(...)` suffix to find the base type and then parses the parameters as needed for size, scale, or inner-type decisions. See Implementation Notes §2.3 for the parsing pattern.

### 3.1 Fixed-width Types

Each value occupies a constant number of bytes. A column of `M` rows occupies exactly `bytes_per_row × M` bytes on the wire, concatenated with no separators or padding.

#### Summary

| Type string         | Bytes per value | Logical value                                     | Wire encoding |
|---------------------|-----------------|---------------------------------------------------|---------------|
| `UInt8`             | 1               | Unsigned 8-bit integer                            | Raw byte      |
| `UInt16`            | 2               | Unsigned 16-bit integer                           | Little-endian |
| `UInt32`            | 4               | Unsigned 32-bit integer                           | Little-endian |
| `UInt64`            | 8               | Unsigned 64-bit integer                           | Little-endian |
| `UInt128`           | 16              | Unsigned 128-bit integer                          | Little-endian |
| `UInt256`           | 32              | Unsigned 256-bit integer                          | Little-endian |
| `Int8`              | 1               | Signed 8-bit, two's complement                    | Raw byte      |
| `Int16`             | 2               | Signed 16-bit, two's complement                   | Little-endian |
| `Int32`             | 4               | Signed 32-bit, two's complement                   | Little-endian |
| `Int64`             | 8               | Signed 64-bit, two's complement                   | Little-endian |
| `Int128`            | 16              | Signed 128-bit, two's complement                  | Little-endian |
| `Int256`            | 32              | Signed 256-bit, two's complement                  | Little-endian |
| `Float32`           | 4               | IEEE 754 single-precision                         | Little-endian |
| `Float64`           | 8               | IEEE 754 double-precision                         | Little-endian |
| `Bool`              | 1               | `0x00` = false, `0x01` = true                     | Raw byte      |
| `Date`              | 2               | Days since `1970-01-01`                           | Little-endian UInt16 |
| `Date32`            | 4               | Days since `1970-01-01` (signed; pre-1970 ok)     | Little-endian Int32 |
| `DateTime`          | 4               | Unix timestamp in seconds                         | Little-endian UInt32 |
| `DateTime(tz)`      | 4               | Same as `DateTime`; timezone is metadata          | Little-endian UInt32 |
| `DateTime64(s)`     | 8               | Ticks at scale `s` (10^-s seconds since epoch)    | Little-endian Int64 |
| `DateTime64(s, tz)` | 8               | Same as `DateTime64(s)`; timezone is metadata     | Little-endian Int64 |
| `UUID`              | 16              | 128-bit identifier                                | Two byte-swapped LE UInt64 halves (see Implementation Notes §2.7) |
| `IPv4`              | 4               | IPv4 address                                      | Little-endian UInt32 |
| `IPv6`              | 16              | IPv6 address                                      | Network byte order, no swap |
| `Enum8`             | 1               | Signed 8-bit (variant index)                      | Raw byte      |
| `Enum16`            | 2               | Signed 16-bit (variant index)                     | Little-endian |
| `Decimal(P, S)`     | 4 / 8 / 16 / 32 | `value × 10^S` as a signed integer; width depends on P (≤9 → 4 B, ≤18 → 8 B, ≤38 → 16 B, ≤76 → 32 B) | Little-endian signed integer |

#### 3.1.1 Integer types

`UInt8`–`UInt256` and `Int8`–`Int256` are direct binary encodings of integer values. A decoder reads `bytes_per_row × num_rows` bytes and interprets them according to the type.

**Byte-level example — `UInt32` column with `[1, 256, 65536]`:**

```
01 00 00 00              row 0: 1
00 01 00 00              row 1: 256
00 00 01 00              row 2: 65536
```

**Byte-level example — `Int32` column with `[-1, 42]`:**

```
FF FF FF FF              row 0: -1
2A 00 00 00              row 1: 42
```

#### 3.1.2 Float32 and Float64

Standard IEEE 754 binary floats — 4 bytes single-precision (`binary32`) and 8 bytes double-precision (`binary64`), each in little-endian byte order. NaN, ±Infinity, ±0.0, and subnormals all round-trip without normalisation.

**Byte-level example — `Float32` value `1.5` (`0x3FC00000`):**

```
00 00 C0 3F              little-endian IEEE 754
```

**Byte-level example — `Float64` value `1.5` (`0x3FF8000000000000`):**

```
00 00 00 00 00 00 F8 3F  little-endian IEEE 754
```

#### 3.1.3 Bool

Wire-compatible with `UInt8` — 1 byte per row, `0x00` = false, `0x01` = true. The type string on the wire is literally `Bool` (not `UInt8`); a decoder dispatching on the type string must recognise it separately.

**Byte-level example — `Bool` column `[true, false, true]`:**

```
01 00 01
```

#### 3.1.4 Date and Date32

Both encode dates as integer day counts relative to the Unix epoch `1970-01-01`. Neither carries a time component.

| Type     | Bytes | Encoding              | Range                           |
|----------|-------|-----------------------|---------------------------------|
| `Date`   | 2     | Little-endian UInt16  | `1970-01-01` to `2149-06-06`    |
| `Date32` | 4     | Little-endian Int32   | wide signed range, pre-1970 ok  |

**Byte-level example — `Date` value `1970-01-02` (1 day):**

```
01 00                    UInt16 LE = 1
```

**Byte-level example — `Date32` value `1900-01-01` (-25567 days):**

```
21 9C FF FF              Int32 LE = -25567
```

#### 3.1.5 DateTime

Wire-compatible with `UInt32` — a Unix timestamp in seconds, 4 bytes little-endian. The type may appear as `DateTime` or `DateTime('Timezone')`; the timezone affects display only and is not part of the wire value. Two `DateTime` columns with different timezone parameters produce identical bytes for the same instant.

A decoder strips the `(...)` parameter suffix and processes the column as `UInt32`.

**Byte-level example — `DateTime('UTC')` value `2024-03-15 14:30:00 UTC` (timestamp `1710513000`):**

```
A8 84 F4 65              UInt32 LE = 1710513000
```

#### 3.1.6 DateTime64(scale[, timezone])

8 bytes, little-endian Int64 representing ticks at scale `10^-scale` seconds since the Unix epoch. The `scale` parameter (0–9) lives in the type string and determines the time unit:

| Scale | Tick size       | Common name |
|-------|-----------------|-------------|
| 0     | 1 second        | seconds     |
| 3     | 1 millisecond   | ms          |
| 6     | 1 microsecond   | µs          |
| 9     | 1 nanosecond    | ns          |

Type-string forms:

- `DateTime64(s)` — implicit server default timezone.
- `DateTime64(s, 'TimezoneName')` — explicit timezone (display only; not part of the wire value).

Negative values represent ticks before the epoch.

**Byte-level example — `DateTime64(3, 'UTC')` value `2024-01-15 12:30:45.123 UTC` (1705321845123 ms):**

```
83 51 1A 0D 8D 01 00 00  Int64 LE = 1705321845123
```

**Byte-level example — `DateTime64(0)` value `2024-01-15 12:30:45 UTC` (1705321845 s):**

```
75 25 A5 65 00 00 00 00  Int64 LE = 1705321845
```

#### 3.1.7 UUID

16 bytes per value. The wire encoding is **not** the canonical 16 big-endian bytes — each 8-byte half is byte-reversed independently. See Implementation Notes §2.7 for context and a worked example.

**Logical model:** a 128-bit identifier in canonical text form `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`, where the bytes are conventionally written big-endian.

**Wire model:** the 16 canonical bytes split into two 8-byte halves; each half is then written little-endian (byte-reversed within the half). Equivalently:

- Wire bytes 0..7 = canonical bytes 0..7 reversed.
- Wire bytes 8..15 = canonical bytes 8..15 reversed.

**Byte-level example — UUID `550e8400-e29b-41d4-a716-446655440000`:**

```
Canonical bytes (16):    55 0E 84 00 E2 9B 41 D4  A7 16 44 66 55 44 00 00

Wire bytes:
D4 41 9B E2 00 84 0E 55  high half byte-reversed
00 00 44 55 66 44 16 A7  low half byte-reversed
```

The nil UUID (all zeros) appears identically in both representations.

#### 3.1.8 IPv4 and IPv6

Two related but differently-encoded address types.

**`IPv4`** — 4 bytes, encoded as a little-endian UInt32 representing the canonical 32-bit address (the value computed by `(a << 24) | (b << 16) | (c << 8) | d` from `a.b.c.d`). The wire bytes are the network-order bytes reversed.

**Byte-level example — `192.168.1.10` (canonical 32-bit value `0xC0A8010A`):**

```
0A 01 A8 C0              Little-endian UInt32
```

**`IPv6`** — 16 bytes, written **verbatim in network byte order** (no swap). Same byte order as `inet_pton(AF_INET6, ...)` and the canonical text representation when expanded.

**Byte-level example — `2001:db8::1`:**

```
20 01 0D B8 00 00 00 00  network bytes 0..7
00 00 00 00 00 00 00 01  network bytes 8..15
```

The asymmetry between IPv4 (LE u32) and IPv6 (raw network bytes) is deliberate — IPv4 is stored as a `u32` for arithmetic and compact range queries, while IPv6 keeps the network-order layout common to most networking APIs.

#### 3.1.9 Enum8 and Enum16

Wire-compatible with `Int8` and `Int16` respectively — 1 or 2 bytes per row, two's complement little-endian for the 16-bit variant. The full variant mapping lives in the type string:

```
Enum8('active' = 1, 'inactive' = 2, 'banned' = -1)
Enum16('a' = 1, 'b' = 30000)
```

A decoder may strip the `(...)` parameter suffix and dispatch as `Int8` / `Int16`. Clients that need the human-readable label parse the type string.

**Byte-level example — `Enum8('active' = 1, 'inactive' = 2)` column `[active, inactive, active]`:**

```
01 02 01
```

**Byte-level example — `Enum16(...)` value `30000`:**

```
30 75                    Int16 LE = 30000
```

#### 3.1.10 Decimal(P, S)

A signed integer scaled by a power of 10. The integer's byte width is implied by the **precision** `P`; the **scale** `S` is the negative exponent (number of digits after the decimal point). Both `P` and `S` are in the type string.

| Precision (P)   | Backing integer | Bytes |
|-----------------|-----------------|-------|
| 1 ≤ P ≤ 9       | Int32           | 4     |
| 10 ≤ P ≤ 18     | Int64           | 8     |
| 19 ≤ P ≤ 38     | Int128          | 16    |
| 39 ≤ P ≤ 76     | Int256          | 32    |

The wire encoding is the backing integer in little-endian, two's complement. The logical decimal value is `wire_integer × 10^(-S)`.

**Type string normalisation.** ClickHouse always emits `Decimal(P, S)` regardless of how the type was declared. `Decimal32(S)`, `Decimal64(S)`, etc. all normalise to `Decimal(P, S)` on the wire (with `P` set to the natural maximum for the width: 9, 18, 38, 76 respectively). A decoder that recognises only `Decimal(P, S)` covers every spelling the server emits.

**Byte-level example — `Decimal(9, 4)` value `123.4567` → backing integer `1234567`:**

```
87 D6 12 00              Int32 LE = 1234567
```

**Byte-level example — `Decimal(18, 1)` value `-1.5` → backing integer `-15`:**

```
F1 FF FF FF FF FF FF FF  Int64 LE = -15
```

**Byte-level example — `Decimal(38, 4)` value `123.4567` (16 bytes total):**

```
87 D6 12 00 00 00 00 00 00 00 00 00 00 00 00 00
```

### 3.2 Variable-length Types

Each value carries its own length on the wire.

#### 3.2.1 String

**Type string:** `String`

**Wire layout.** A sequence of `num_rows` length-prefixed byte sequences:

```
[VarUInt: byte_length] [byte_length bytes: raw value]
[VarUInt: byte_length] [byte_length bytes: raw value]
...
```

There are no separators between rows beyond the length prefixes and no row-level state. Empty strings are a single `0x00` byte. Strings may contain any byte values, including embedded NUL.

**Invariants.** Total bytes consumed by a String column is `Σ (varuint_size(len_i) + len_i)` for `i` in `0..num_rows`.

**Encoding semantics.** ClickHouse `String` is byte-oriented, not text-oriented. UTF-8 validity is not enforced. A decoder that targets a UTF-8 string type either validates on read or exposes raw bytes for the caller.

**Byte-level example — column of 3 strings `["ab", "", "c"]`:**

```
02 61 62                 row 0: length 2, "ab"
00                       row 1: length 0, empty
01 63                    row 2: length 1, "c"
```

Total: 6 bytes of column data.

#### 3.2.2 FixedString(N)

**Type string:** `FixedString(N)` where `N` is a positive integer (e.g., `FixedString(16)`).

**Wire layout.** Exactly `N × num_rows` raw bytes — no length prefixes, no separators. The decoder parses `N` from the type string and consumes that many bytes per row.

**Padding.** When the SQL inserts a value shorter than `N` bytes (e.g., `CAST('abc' AS FixedString(5))`), the server right-pads with NUL bytes (`0x00`) to the declared length. These padding bytes are part of the stored value and are sent on the wire as-is. Trimming is a client-side concern.

**Encoding semantics.** `FixedString(N)` is byte-array-like, not text-like — typically used for fixed-width identifiers, IP-address-shaped bytes, or hash digests. UTF-8 should not be assumed.

**Byte-level example — 2 `FixedString(3)` values `["abc", "de\0"]`:**

```
61 62 63                 row 0: 3 bytes, "abc"
64 65 00                 row 1: 3 bytes, "de" + NUL padding
```

Total: 6 bytes of column data.

#### Comparison

| Property               | `String`              | `FixedString(N)`            |
|------------------------|-----------------------|-----------------------------|
| Per-row length prefix  | Yes (VarUInt)         | No                          |
| Row size               | Variable              | Exactly `N` bytes           |
| Total column bytes     | Variable              | `N × num_rows`              |
| NUL-byte padding       | n/a                   | Right-padded by server      |
| UTF-8 expected         | Typically (not enforced) | No (treat as raw bytes)  |
| Type parameter         | None                  | Required integer `N`        |

### 3.3 Composite Types

Composite types are built by wrapping one or more inner types. They share a common wire model: **multiple streams per column** — a single logical column on the wire is encoded as two or more independently-read sequences of bytes, concatenated.

These types share three structural properties:

- **Fixed shape per schema.** The structure is determined entirely by the type string at decode time. `Array(UInt32)` always has the same stream layout regardless of block.
- **No version prefix.** The stream layout is stable across ClickHouse releases.
- **No cross-block state.** Each block is fully self-describing.

Composites are recursive — inner types may themselves be composites.

#### 3.3.1 Nullable(T)

**Type string syntax:** `Nullable(InnerType)`. Examples: `Nullable(UInt32)`, `Nullable(String)`, `Nullable(FixedString(16))`, `Nullable(DateTime('UTC'))`.

**Wire layout:** two concatenated streams, null-map first.

```
[null-map stream]  num_rows × UInt8
[values stream]    inner type's encoding for num_rows values
```

The null-map is exactly `num_rows` bytes. Each byte indicates whether the corresponding row is null:

| Byte value | Meaning |
|------------|---------|
| `0x00`     | Value is present at this row. |
| non-zero (canonical `0x01`) | Value is NULL. The corresponding bytes in the values stream are a placeholder. |

The values stream contains the inner type's standard encoding for **all** `num_rows` rows, including the null positions. Values at null positions are placeholder bytes — the decoder must still read them to advance the stream, but must consult the null-map before interpreting any individual value.

**Invariants.**

1. The null-map has exactly `num_rows` bytes.
2. The values stream has exactly `num_rows` values.
3. `Nullable(Nullable(T))` is rejected by the server. Nullability does not compose with itself.

**Placeholder values by inner type:**

| Inner type family                                    | Placeholder at null position |
|------------------------------------------------------|------------------------------|
| Fixed-width (UInt/Int/Float/DateTime/UUID/etc.)      | Zero-initialised bytes of the type's width |
| `String`                                             | Empty string — single `0x00` byte |
| `FixedString(N)`                                     | `N` zero bytes |
| `Array(T)`                                           | Empty array — offsets advance by zero |
| `Tuple(T1, T2, ...)`                                 | Each element uses its own placeholder |

Senders may write any bytes at null positions. Decoders must not rely on any specific placeholder value.

**Composes with.** `Nullable(T)` may appear inside `Array`, `Tuple`, `Map`, and `Nested`. `Array(Nullable(T))` and `Tuple(Nullable(T1), T2)` are common. `Nullable(Nullable(T))` is forbidden.

**Byte-level example — `Nullable(UInt8)` with three rows `[5, NULL, 9]`:**

```
00 01 00                 null-map: present, null, present
05 00 09                 values:   5, placeholder, 9
```

Total: 6 bytes of column data.

**Byte-level example — `Nullable(String)` with three rows `["hello", NULL, "world"]`:**

```
00 01 00                 null-map
05 'h' 'e' 'l' 'l' 'o'   row 0: "hello"
00                       row 1: placeholder (empty string)
05 'w' 'o' 'r' 'l' 'd'   row 2: "world"
```

Total: 15 bytes of column data.

#### 3.3.2 Array(T)

**Type string syntax:** `Array(InnerType)`. Examples: `Array(UInt32)`, `Array(String)`, `Array(Nullable(UInt32))`, `Array(Array(UInt8))`.

**Wire layout:** two concatenated streams, offsets first.

```
[offsets stream]   num_rows × UInt64 LE
[values stream]    inner type's encoding for offsets[num_rows - 1] values
```

The offsets stream contains exactly `num_rows` little-endian UInt64 values. Each offset is the **cumulative end position** in the values stream after that row's elements:

- Element start index for row `N` = `offsets[N - 1]` (or `0` when `N == 0`).
- Element end index (exclusive) for row `N` = `offsets[N]`.
- Row `N`'s element count = `offsets[N] - offsets[N - 1]`.

`offsets[num_rows - 1]` equals the total element count across all rows.

The values stream contains the inner type's standard encoding for all `offsets[num_rows - 1]` values, concatenated end-to-end.

**Invariants.**

1. Offsets are **monotonic non-decreasing**. Equal consecutive offsets mean an empty row.
2. The values stream contains exactly `offsets[num_rows - 1]` values.
3. An empty column (`num_rows == 0`) writes zero bytes — no offsets stream, no values stream.

Decoders reject non-monotonic offsets as corruption.

**Composes with.** Inner types may be any type, including other composites. `Array(Array(T))`, `Array(Tuple(...))`, `Array(Nullable(T))` are all legal.

**Byte-level example — `Array(UInt32)` with rows `[[10, 20, 30], [], [40, 50]]`:**

```
Offsets (3 × UInt64 LE = 24 bytes):
03 00 00 00 00 00 00 00      offsets[0] = 3
03 00 00 00 00 00 00 00      offsets[1] = 3 (empty row)
05 00 00 00 00 00 00 00      offsets[2] = 5

Values (5 × UInt32 LE = 20 bytes):
0A 00 00 00                  10
14 00 00 00                  20
1E 00 00 00                  30
28 00 00 00                  40
32 00 00 00                  50
```

Total: 44 bytes of column data.

**Byte-level example — `Array(String)` with rows `[["a", "bb"], []]`:**

```
Offsets (2 × UInt64 LE = 16 bytes):
02 00 00 00 00 00 00 00      offsets[0] = 2
02 00 00 00 00 00 00 00      offsets[1] = 2 (empty row)

Values (2 strings, 4 bytes total):
01 'a'                       row's first string: "a"
02 'b' 'b'                   row's second string: "bb"
```

Total: 20 bytes.

**Byte-level example — `Array(Array(UInt32))` with rows `[[[1,2]], [], [[3], [4,5]]]`:**

- Outer offsets: `[1, 1, 3]` — row 0 has 1 inner-array, row 1 has 0, row 2 has 2.
- Middle `Array(UInt32)` decodes 3 rows; offsets `[2, 3, 5]`.
- Innermost `UInt32` decodes 5 values: `[1, 2, 3, 4, 5]`.

Total: 24 (outer offsets) + 24 (middle offsets) + 20 (values) = 68 bytes.

#### 3.3.3 Tuple(T1, T2, ...)

**Type string syntax:** `Tuple(T1, T2, ..., Tn)`. Examples: `Tuple(UInt32, String)`, `Tuple(Int32)`, `Tuple(Array(UInt32), String)`, `Tuple(UInt8, Tuple(Int32, String))`.

ClickHouse also supports **named tuples** via `Tuple(a UInt32, b String)`. Names are metadata only and do not affect the wire format.

**Wire layout:** *N* concatenated streams, one per element type, in declaration order.

```
[stream for T1]    inner T1's encoding for num_rows values
[stream for T2]    inner T2's encoding for num_rows values
 ...
[stream for Tn]    inner Tn's encoding for num_rows values
```

Each stream encodes exactly `num_rows` values. There is no length prefix, no offsets stream, no separators between streams.

**Invariants.**

1. Tuple has at least one element (`n >= 1`). Empty tuples are rejected at type-parse time.
2. Every element stream encodes exactly `num_rows` values.
3. An empty column (`num_rows == 0`) writes zero bytes per stream.

**Composes with.** Element types may be any type, including other composites. `Tuple(Tuple(...), ...)`, `Tuple(Array(...), ...)`, `Tuple(Nullable(T1), T2)` are all legal. The depth-aware comma splitter described in Implementation Notes §2.6 is required to parse the element list.

**Byte-level example — `Tuple(UInt8, UInt8)` with 3 rows `(1,4), (2,5), (3,6)`:**

```
Element 0 stream (3 × UInt8 = 3 bytes):
01 02 03

Element 1 stream (3 × UInt8 = 3 bytes):
04 05 06
```

Total: 6 bytes. The order is **not** row-major — reading the raw bytes back yields `[1, 2, 3]` for element 0 and `[4, 5, 6]` for element 1.

**Byte-level example — `Tuple(UInt32, String)` with 2 rows `(10, "a")`, `(20, "bb")`:**

```
Element 0 stream (2 × UInt32 LE = 8 bytes):
0A 00 00 00                  10
14 00 00 00                  20

Element 1 stream (2 strings, 5 bytes total):
01 'a'                       "a"
02 'b' 'b'                   "bb"
```

Total: 13 bytes.

#### 3.3.4 Map(K, V)

**Type string syntax:** `Map(KeyType, ValueType)`. Examples: `Map(String, UInt32)`, `Map(String, Array(UInt32))`, `Map(UInt8, Tuple(Int32, String))`, `Map(Array(String), Int8)`.

The wire format imposes no restriction on either type — both `K` and `V` may be any type the encoder/decoder supports, including composites. ClickHouse's SQL-level rules around accepted key types have varied across releases; consult ClickHouse documentation for the SQL rules of the targeted server version.

**Wire layout:** byte-identical to `Array(Tuple(K, V))`.

```
[offsets stream]    num_rows × UInt64 LE                   ← from Array
[keys stream]       K's encoding for total_pairs values    ┐ from Tuple's
[values stream]     V's encoding for total_pairs values    ┘ per-element streams
```

where `total_pairs = offsets[num_rows - 1]` (or `0` when `num_rows == 0`).

The offsets stream has the same semantics as `Array` (§3.3.2). Keys are positionally aligned with values: pair `i` is `(keys[i], values[i])`.

**Invariants.**

1. Offsets are monotonic non-decreasing.
2. Both keys stream and values stream contain exactly `total_pairs` values.
3. An empty column (`num_rows == 0`) writes zero bytes.
4. *(Semantic, not wire-enforced.)* Within a single row, keys are typically unique. The wire format permits duplicate keys to round-trip — server-side semantics resolve duplicates only when the row is consumed by a Map-aware function.

Decoders reject non-monotonic offsets.

**Equivalence to Array(Tuple(K, V)).** ClickHouse's in-memory representation of a Map column is an Array of Tuples; the type system surfaces it as a distinct type for SQL ergonomics (`m['key']`, `mapKeys`, `mapValues`). The wire format is a direct serialization of that storage, so Map and `Array(Tuple(K, V))` are byte-for-byte interchangeable.

**Composes with.** Both K and V may be any type, including composites. Wire bytes are identical to `Array(Tuple(K, V))` regardless of how the inner types nest.

**Byte-level example — `Map(UInt8, UInt8)` with 2 rows `{1:10, 2:20}`, `{3:30}`:**

```
Offsets (2 × UInt64 LE = 16 bytes):
02 00 00 00 00 00 00 00      offsets[0] = 2
03 00 00 00 00 00 00 00      offsets[1] = 3

Keys (3 × UInt8 = 3 bytes):
01 02 03                     keys: 1, 2, 3

Values (3 × UInt8 = 3 bytes):
0A 14 1E                     values: 10, 20, 30
```

Total: 22 bytes. Keys and values are stored as separate streams, not interleaved — pair `i` is reconstructed by reading `keys[i]` and `values[i]` and pairing them.

**Byte-level example — `Map(String, UInt32)` with 1 row `{'a':1, 'b':2}`:**

```
Offsets (1 × UInt64 LE = 8 bytes):
02 00 00 00 00 00 00 00      offsets[0] = 2

Keys (2 strings, 4 bytes total):
01 'a'                       "a"
01 'b'                       "b"

Values (2 × UInt32 LE = 8 bytes):
01 00 00 00                  1
02 00 00 00                  2
```

Total: 20 bytes.

#### 3.3.5 Nested(name1 T1, name2 T2, ...)

The `Nested` type's on-wire representation depends on the server-side `flatten_nested` setting. There are two distinct cases.

**Case A: `flatten_nested = 1` (server default).**

When the table was created under default settings, `Nested` is **not a wire type**. The server stores and presents the column as N parallel `Array(T_i)` columns with **dotted names** (`outer.field1`, `outer.field2`, …). For the format layer there is nothing new — every dotted column is a regular `Array(T)` (§3.3.2).

Empirical confirmation:

```
DESCRIBE TABLE t   -- t has column n Nested(a UInt8, b String)
id     UInt8
n.a    Array(UInt8)
n.b    Array(String)
```

**Case B: `flatten_nested = 0`.**

When the table was created with `flatten_nested = 0`, the column appears on the wire as a single column with type string `Nested(name1 T1, name2 T2, ...)`. The wire format is **byte-identical to `Array(Tuple(T1, T2, ..., Tn))`** after the type string. Empirically verified by hex-comparing two SELECTs against the same data:

```
Nested(a UInt8, b String) bytes (after type string):
  02 00 00 00 00 00 00 00       offsets[0] = 2
  03 00 00 00 00 00 00 00       offsets[1] = 3
  0A 14 1E                       UInt8 stream
  01 'x' 01 'y' 01 'z'           String stream

Array(Tuple(a UInt8, b String)) bytes (after type string):
  02 00 00 00 00 00 00 00       offsets[0] = 2
  03 00 00 00 00 00 00 00       offsets[1] = 3
  0A 14 1E                       UInt8 stream
  01 'x' 01 'y' 01 'z'           String stream
```

The only difference is the type-string text — `Nested` preserves the field names (`a`, `b`) which `Array(Tuple)` does not carry as named slots.

**Type-string syntax (Case B):** `Nested(name1 TYPE1, name2 TYPE2, ...)` — a comma-separated list of (name, type) pairs. The first whitespace separates a name from its type; the type itself may contain further whitespace, commas, and parens. Backtick-quoted identifiers are syntactically legal in SQL but rare in protocol traffic.

**Wire layout (Case B):**

```
[offsets stream]    num_rows × UInt64 LE                       ← from Array
[field1 stream]     T1's encoding for total_elements values    ┐ from Tuple's
[field2 stream]     T2's encoding for total_elements values    │ per-element
 ...                                                            │ streams
[fieldn stream]     Tn's encoding for total_elements values    ┘
```

where `total_elements = offsets[num_rows - 1]` (or `0` when `num_rows == 0`).

**Invariants.**

1. Offsets are monotonic non-decreasing.
2. Every field stream contains exactly `total_elements` values.
3. *(Server-enforced at INSERT.)* Within a single row, all fields carry the same number of elements.
4. An empty column writes zero bytes.

**Byte-level example — `Nested(a UInt8, b String)` with 2 rows `[(10,'x'),(20,'y')]` and `[(30,'z')]`:**

```
Offsets (2 × UInt64 LE = 16 bytes):
02 00 00 00 00 00 00 00      offsets[0] = 2
03 00 00 00 00 00 00 00      offsets[1] = 3

Field 'a' stream (3 × UInt8 = 3 bytes):
0A 14 1E                     10, 20, 30

Field 'b' stream (3 strings, 6 bytes):
01 'x' 01 'y' 01 'z'         "x", "y", "z"
```

Total: 25 bytes after the type string.

### 3.4 Versioned Types

Versioned types carry an on-wire serialisation-version prefix that declares which variant of the encoding follows. They may also use multiple streams (like §3.3) and may maintain cross-block state.

Implementing versioned types is an order of magnitude more complex than fixed-shape composites. A client targeting simple analytical queries can defer this section.

#### 3.4.1 Serialisation version: concept

A **serialisation version** is a per-type, per-column on-wire version number that declares which variant of the type's encoding the sender is using.

The serialisation version is the first thing in the column's state prefix, so the decoder reads it and dispatches to the right parser for the rest of the column.

The serialisation version is **distinct from** the protocol version (`NATIVE_PROTOCOL.md` §3):

| Dimension              | Protocol version           | Serialisation version (this section) |
|------------------------|----------------------------|--------------------------------------|
| Scope                  | Connection-wide            | Per-type, per-column                 |
| Negotiated             | Yes, at handshake          | No — sender writes, receiver reads   |
| Controls               | Which packet-level features are active | Which wire variant of one type     |
| Mandatory to read      | Yes                        | Yes, for each versioned column       |

Most versioned types write the version as a little-endian UInt64 immediately before any other state-prefix data. A few use VarUInt or UInt8 — the exact width is per-type.

A decoder reads the version first and rejects unknown values — a higher version implies a newer sender format the current decoder does not understand. Mis-parsing corrupts every subsequent byte.

#### 3.4.2 Serialisation version reference

| Type | Field width | Value | Name | Meaning |
|---|---|---|---|---|
| **Object** (base for JSON) | UInt64 LE | `0` | `V1` | Original encoding. Includes `max_dynamic_paths` parameter and a list of dynamic paths. |
| | | `1` | `STRING` | Native-format compatibility mode — Object transmitted as a single `String` column containing JSON text. |
| | | `2` | `V2` | V1 layout minus the `max_dynamic_paths` parameter. |
| | | `3` | `FLATTENED` | Native-format compatibility mode — flattened path representation. |
| | | `4` | `V3` | V2 plus a shared-data serialisation version sub-field and a statistics flag. |
| **Object shared data** (sub-stream used in Object `V3`) | VarUInt | `0` | `MAP` | Shared data encoded as `Map(String, String)`. |
| | | `1` | `MAP_WITH_BUCKETS` | Same as `MAP` but split into N buckets for scan efficiency. |
| | | `2` | `ADVANCED` | Compact granule format with separate streams for paths / marks / metadata. |
| **Dynamic** | UInt64 LE | `1` | `V1` | Original encoding. Includes `max_dynamic_types` and a list of runtime variant types. |
| | | `2` | `V2` | V1 minus the `max_dynamic_types` parameter. |
| | | `3` | `FLATTENED` | Native-format compatibility mode. |
| | | `4` | `V3` | V2 plus binary-encoded variant type names and empty-statistics support. |
| **Variant** discriminators mode | UInt64 LE | `0` | `BASIC` | Every row's discriminator is written literally. |
| | | `1` | `COMPACT` | If all rows in a granule share one discriminator, only a single value + granule marker is written. |
| **Variant** granule format (when mode is `COMPACT`) | UInt8 | `0` | `PLAIN` | Granule has heterogeneous discriminators. |
| | | `1` | `COMPACT` | Granule has one discriminator for all rows. |
| **LowCardinality** key serialisation | Int64 | `1` | `sharedDictionariesWithAdditionalKeys` | Only version currently defined. |
| **JSON-as-String** fallback (when `output_format_native_write_json_as_string` is enabled) | UInt64 LE | `1` | `JSONStringSerializationVersion` | JSON column arrives as a `String` column preceded by this prefix. |

Notes on the table:

- **Values are not contiguous.** `Dynamic` uses values `1`, `2`, `3`, `4` with `V3` at `4` and `FLATTENED` at `3`. Higher numbers are not necessarily newer.
- **Native-format-only values.** `Object::STRING`, `Object::FLATTENED`, `Dynamic::FLATTENED` exist for native-protocol compatibility with clients that do not implement full Object/Dynamic. They do not appear in MergeTree on-disk storage.
- **`V3` is primarily on-disk.** Clients consuming the native TCP protocol typically see `FLATTENED` (value `3`) instead of `V3` (value `4`).

#### 3.4.3 LowCardinality(T)

The simplest versioned type. Replaces a column of `N` inner values with a small dictionary of unique values plus `N` indices into that dictionary.

**Type string syntax:** `LowCardinality(InnerType)`. Examples: `LowCardinality(String)`, `LowCardinality(FixedString(4))`, `LowCardinality(Nullable(String))`.

**Wire layout:**

```
[8 bytes:  Int64 LE state prefix = 1]               ← once per column per query
                                                      only emitted before the first block with rows > 0
[per block with rows > 0]:
  [8 bytes:  UInt64 LE metadata]                    ← key type code (low byte) + flag bits
  [8 bytes:  UInt64 LE dict_size]                   ← number of dict entries (incl. placeholder slot)
  [N bytes:  dict values]                           ← inner type's encoding for dict_size values
  [8 bytes:  UInt64 LE keys_count]                  ← always equal to this block's row count
  [K bytes:  keys]                                  ← (1 << key_type_code) bytes per key
```

**State prefix** (Int64 LE = 1) is the single defined version, `sharedDictionariesWithAdditionalKeys`. Other values are reserved.

**Per-block metadata UInt64:**

| Bit range    | Meaning |
|--------------|---------|
| 0..7         | Key type code: `0` = UInt8, `1` = UInt16, `2` = UInt32, `3` = UInt64. The smallest type that can index `dict_size` entries is chosen. |
| 9 (`0x200`)  | `HasAdditionalKeysBit` — set when the block contains new dict entries. |
| 10 (`0x400`) | `NeedUpdateDictionary` — set when the dict in this block extends the global dict. |
| 11 (`0x800`) | `NeedGlobalDictionaryBit` — set when this block references entries from a global dict shared across blocks. |

For typical query responses with a single data block per column, the metadata is `0x600` (HasAdditionalKeys + NeedUpdateDictionary).

**Dict values** are `dict_size` values encoded using the inner type T. By convention `dict[0]` is an empty/default placeholder. For `LowCardinality(Nullable(T))`, the wire encodes the dict as plain T (no null-map stream); `dict[1]` is the null marker, and real values start at `dict[2..]`.

**Keys** are indices into the dict. `keys.len() == this block's row count`. Each index is `1 << key_type_code` bytes (1, 2, 4, or 8). Logical row `N` is reconstructed as `dict[keys[N]]`.

**Invariants.**

1. State prefix is read once per column per query, before the first block whose row count is greater than zero. Header blocks (rows = 0) and empty blocks emit nothing.
2. `keys_count` equals the block's row count.
3. `dict_size` equals the number of values encoded in the dict stream.
4. Each key fits in `1 << key_type_code` bytes.

See Implementation Notes §2.8 for the cross-block-state caveat that affects multi-block queries.

**Composes with.** `LowCardinality(Nullable(T))` is the common case. Wrapping `LowCardinality` in other composites (`Array(LowCardinality(T))`, etc.) produces a stream where the inner LowCardinality column's state prefix and per-block data appear at the position the outer composite delegates to.

**Byte-level example — `LowCardinality(String)` with values `['a', 'b', 'a', 'c', 'b']`:**

```
01 00 00 00 00 00 00 00      state prefix Int64 = 1
00 06 00 00 00 00 00 00      metadata UInt64 = 0x600
04 00 00 00 00 00 00 00      dict_size = 4
00                           dict[0] = "" (placeholder)
01 'a'                       dict[1] = "a"
01 'b'                       dict[2] = "b"
01 'c'                       dict[3] = "c"
05 00 00 00 00 00 00 00      keys_count = 5
01 02 01 03 02               keys (UInt8): 1, 2, 1, 3, 2
```

Reconstructed: `dict[1], dict[2], dict[1], dict[3], dict[2]` = `["a", "b", "a", "c", "b"]`.

#### 3.4.4 JSON (Tier 1: String fallback)

ClickHouse's `JSON` type has multiple wire encodings (see §3.4.2). Tier 1 is the simplest: when the per-query setting `output_format_native_write_json_as_string = 1` is set, the server flattens every JSON value to its serialised text and emits the column as a `String` with a state-prefix marker.

**Type string syntax:** `JSON`.

**Wire layout (Tier 1):**

```
[8 bytes:  Int64 LE state prefix = 1]        ← JSONStringSerializationVersion
[per block with rows > 0]:
  [N bytes: String column encoding for num_rows JSON text values]
```

**State prefix value:** `1` (`JSONStringSerializationVersion`). Other values (`0`, `3`, `4`) indicate FLATTENED / V3 formats — see §3.4.5.

**Invariants.**

1. State prefix is read once per column per query, before the first block with rows > 0.
2. The values stream is a standard String column for `num_rows` rows (§3.2.1).

**Byte-level example — `JSON` value `'{"a":1}'` (one row, scale rendered as JSON text):**

```
01 00 00 00 00 00 00 00      state prefix Int64 = 1
09 7B 22 61 22 3A 22 31 22   String: 9 bytes "{"a":"1"}"
7D
```

Note that ClickHouse re-stringifies non-string JSON values when emitting in Tier 1 mode — the integer `1` becomes the JSON string `"1"`. Tier 1 is sufficient for queries where the client receives JSON for opaque transit; faithful round-tripping of types requires Tier 2 (§3.4.5).

#### 3.4.5 Variant, Dynamic, JSON Tier 2/3 — out of scope for this revision

Three closely-related types are not specified in detail here:

- **`Variant(T1, T2, ...)`** — discriminated union. Each row has a discriminator (UInt8 in BASIC mode, more complex in COMPACT mode) selecting which sub-column carries that row's value. `255` is the `NULL_DISCRIMINATOR`.
- **`Dynamic`** — runtime-typed column. State prefix carries a serialisation version (`V1=1`, `V2=2`, `V3=4`, `FLATTENED=3`); then a list of variant type names discovered at runtime; then a `Variant` encoding using those types. Type list grows across blocks within a query.
- **`JSON` Tier 2 (FLATTENED) and Tier 3 (V3)** — `Object`-rooted format: a list of dynamic paths, each path encoded as a `Dynamic` column, plus a shared-data column.

These types share the structural pattern of state-prefix + per-block payload but introduce multi-block cross-column state and discriminator dispatch. Comprehensive specifications for them are deferred to a future revision of this document.

---

## 4. Compression Frame

ClickHouse supports per-block compression for the column data carried inside Data, Totals, Extremes, Log, and ProfileEvents packets. Compression is opt-in and activated by the protocol-level `compression` flag in the Query packet (`NATIVE_PROTOCOL.md` §6).

When compression is active, every Block body (the bytes after the `table_name` string of a Data-family packet) is wrapped in the frame defined below. The packet envelope itself (packet type code, table_name string, BlockInfo prefix) is **not** compressed — only the columnar payload.

### 4.1 Frame format

```
[16 bytes: CityHash128 checksum over the 9-byte header + compressed body]
[1 byte:   method]                 ← 0x82 = LZ4, 0x90 = ZSTD, 0x02 = NONE
[4 bytes:  compressed_size LE u32] ← INCLUDES the 9-byte header, EXCLUDES the 16-byte checksum
[4 bytes:  uncompressed_size LE u32]
[N bytes:  compressed body]        ← N = compressed_size - 9
```

Total framed size: `16 + compressed_size` = `16 + 9 + body_size` = `25 + body_size`.

### 4.2 Method byte values

| Byte   | Method | Body encoding |
|--------|--------|---------------|
| `0x02` | NONE   | Body is the raw bytes (no compression). The frame is still emitted; the receiver verifies the checksum. |
| `0x82` | LZ4    | Body is the **LZ4 block format** — *not* the LZ4 frame format. No magic number. |
| `0x90` | ZSTD   | Body is a raw zstd single-frame stream (the standard zstd magic number is part of the body). |

### 4.3 Checksum

ClickHouse uses CityHash v1.0.2 (the historical variant), **not** modern Google CityHash. The two produce different outputs.

The checksum is computed over the 9 header bytes (method + compressed_size + uncompressed_size) plus the N body bytes — i.e., everything between the checksum and the end of the frame. The first 8 bytes of the 16-byte CityHash128 output are the low half (LE), the next 8 bytes are the high half (LE).

A decoder recomputes the CityHash128 over the received header+body and compares against the leading 16 bytes. Mismatch is corruption — the decoder fails.

### 4.4 Per-block boundaries

Each Block is its own frame. A query response with multiple Data packets contains one frame per packet. There is no enclosing multi-block frame.

The frame's `compressed_size` and `uncompressed_size` are independent counters — the sender pre-compresses, then writes the framing prefix, then the compressed bytes. Receivers stream the frame: read 16 + 9 bytes, then read exactly `compressed_size - 9` body bytes, then decompress to exactly `uncompressed_size` bytes.

### 4.5 Negotiation

Compression is per-query, not per-connection. The protocol-level Query packet's `compression: bool` field requests it for that single query. The server honours the request and emits compressed Data/Totals/Extremes/Log/ProfileEvents bodies for the lifetime of the query. Subsequent queries on the same connection may differ.

---

## 5. Glossary

**Block** — the unit of data exchange in the Native format. A self-describing chunk of rows organised columnar. See §2.

**BlockInfo** — metadata header that precedes a Block when the protocol-level `BLOCK_INFO` feature is active (v51903+). Field-tagged for forward compatibility. See §2.2.

**Column body** — the bytes of a Column that hold the actual values, after the column header (name, type, has_custom_serialization byte). Layout is type-specific. See §2.3.

**Composite type** — a type built from one or more inner types, encoded as multiple streams per column. Wire format is stable and unversioned. See §3.3.

**Dictionary (LowCardinality)** — the array of unique values that a `LowCardinality(T)` column references via integer indices. See §3.4.3.

**Empty block** — a Block with `num_columns = 0` and `num_rows = 0`. Used as a sentinel: client-side end-of-input marker, server-side stream boundary marker. See §2.4.

**Header block** — a Block with `num_columns > 0` and `num_rows = 0`, sent by the server as the first Data packet of a query response. Announces the result schema. See §2.4.

**Inner type** — the type a composite wraps. `Array(UInt32)` has inner type `UInt32`; `Nullable(T)`'s inner type is `T`.

**Offsets stream** — the cumulative-end-position UInt64 array used by `Array`, `Map`, and `Nested` to delimit per-row element boundaries. See §3.3.2.

**Placeholder value** — bytes written at null positions in a `Nullable(T)` column's values stream. The decoder reads them to advance the stream but ignores their content. See §3.3.1.

**Result block** — a Block with `num_rows > 0` carrying actual query result rows. See §2.4.

**Schema block** — synonym for header block (§2.4). The term used when describing the INSERT phase, where the schema block tells the client the expected column shapes.

**Serialisation version** — a per-type on-wire version number used by versioned types to declare which variant of the encoding follows. Distinct from the protocol version. See §3.4.1.

**State prefix** — bytes preceding the per-block payload of a versioned type. Carries the serialisation version and (for LowCardinality) one-time-per-column metadata. Emitted once per column per query, before the first block with rows > 0.

**Stream** — a contiguous run of bytes within a column body, encoding one logical sub-component (e.g., a null-map, an offsets array, a values stream). Multi-stream types (composites and versioned types) concatenate two or more streams per column.
