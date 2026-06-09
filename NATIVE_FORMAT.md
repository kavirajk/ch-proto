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
| 4 | kind_stack               | bytes   | when field 3 = `1`       | One UInt8 enum byte (see §2.3.1) describing the non-default serialization (sparse, etc.). For the `COMBINATION` value, followed by a VarUInt count + that many additional kind bytes. |
| 5 | data                     | bytes   | always                   | Column values for all `num_rows` rows. Layout per type — see §3. For sparse columns, see §2.3.1. |

#### 2.3.1 kind_stack and sparse encoding

The `kind_stack` byte enumerates a non-default per-column serialization. Mirrors `KindStackBinarySerializationType` in `ClickHouse/src/DataTypes/Serializations/SerializationInfo.cpp`:

| Byte | Name | Meaning | Wire impact on `data` |
|------|------|---------|------------------------|
| `0x00` | DEFAULT | Default serialization | Identical to `has_custom = 0` |
| `0x01` | SPARSE | Sparse serialization (v54465+) | Offset stream + non-default values; see below |
| `0x02` | DETACHED | Internal storage form (not used over the wire) | — |
| `0x03` | DETACHED_OVER_SPARSE | Detached over sparse (not used over the wire) | — |
| `0x04` | REPLICATED | Dictionary form for repeated values (v54482+) | Index stream + dense element values; see below |
| `0x05` | COMBINATION | Multi-kind stack | Followed by VarUInt `count` and `count` further kind bytes |

**Sparse wire format.** When `kind_stack = 0x01`, the column `data` is two streams written back-to-back in the single shared TCP stream:

1. **Offset stream** — a sequence of `VarUInt`s. Each `VarUInt` value `v` is either:
   - `v` with the high bit at position 62 clear: `(v & 0x3FFFFFFFFFFFFFFF)` = number of default positions before the next explicit non-default. The non-default position is computed as `cursor + group_size` where `cursor` is the running position; afterwards `cursor` advances by `group_size + 1`.
   - `v` with bit 62 set (`END_OF_GRANULE_FLAG`): the value with the flag cleared = number of trailing default positions after the last non-default. Marks end of the offset stream for this Block.
2. **Values stream** — `count` non-default values densely encoded in the inner type, where `count` is the number of non-EOG VarUInts read above.

Decoders reconstruct a dense column of `num_rows` entries by filling every non-explicit position with the inner type's zero value (`0` for integers/floats, `""` for `String`, `0` days for `Date`, etc.).

**Replicated wire format.** When `kind_stack = 0x04`, the column `data` is a dictionary: a list of distinct element values plus a per-row index into that list (the same lookup shape as `LowCardinality`). Layout:

```
[VarUInt num_rows]
[UInt8  size_of_indexes_type]            width of each index: 1, 2, 4, or 8 bytes
[indexes: num_rows × size_of_indexes_type bytes]
[VarUInt num_elements]
[elements: num_elements dense inner-type values]
```

Decoders reconstruct a dense column by selecting `elements[indexes[i]]` for each output row `i`. Composite inner types recurse: the element list is materialized in the inner type, then indexed. Supported inners include the leaf types, `Nullable(T)`, `Array(T)`, `Tuple(...)`, `Map(K, V)`, `Nested(...)` (each field expanded like an `Array`), and `LowCardinality(T)` (the shared dictionary is kept; only the per-element keys are indexed).

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

A decoder may strip the `(...)` parameter suffix and dispatch as `Int8` / `Int16`. Clients that need the human-readable label parse the label↔value map out of the type string and keep it alongside the values — text output (TabSeparated, etc.) renders the **label** (`active`), not the integer; inside composites it is single-quoted (`'active'`). Because the map is not recoverable from the integer column alone, it must be retained for nested enums such as `Array(Enum8(...))` or `Map(Enum16(...), V)`.

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

#### 3.1.8 Nothing

The `Nothing` type carries no values. It appears in practice only as the inner type of `Nullable(Nothing)` — what the server returns for expressions like `SELECT NULL` whose only valid value is the absence of a value. Conceptually a unit type.

**Wire format.** Exactly **one placeholder byte per row**. The canonical server emits the ASCII character `'0'` (`0x30`); the deserializer ignores the bytes. The byte content is undefined and decoders MUST NOT rely on any specific value. The number of bytes written is `num_rows × 1`, so the column header's `num_rows` field fully determines how much to consume.

**Why a byte per row at all?** The Block structure requires every column to span `bytes_per_value × num_rows` (or its equivalent for variable types) so that decoders can scan forward without per-cell length prefixes. `Nothing` keeps that invariant intact while carrying no information; the surrounding `Nullable` always reports every position as NULL, so the placeholders are never inspected.

**Byte-level example — `Nullable(Nothing)` column with 3 rows (all NULL):**

```
01 01 01                 null map: 1, 1, 1 (three NULLs)
30 30 30                 Nothing placeholder bytes (one per row)
```

The null-map prefix is the standard `Nullable` framing (§3.3); the inner three bytes are the `Nothing` payload and would be skipped by the decoder.

#### 3.1.12 BFloat16

`BFloat16` is the brain-floating-point format: the high 16 bits of an IEEE-754 `Float32` (1 sign, 8 exponent, 7 mantissa bits). **Wire format:** 2 bytes LE per row — the raw 16-bit pattern. To obtain the value, widen to `Float32` by shifting the pattern into the high half (`f32::from_bits((bits as u32) << 16)`); text output uses the same float formatting as `Float32` (§2.9 of Implementation Notes). Example: `1.5` → bits `0x3FC0` → wire `c0 3f`.

#### 3.1.13 Time and Time64(scale)

`Time` is a signed clock duration in **seconds**, `Int32` LE (4 bytes). `Time64(scale)` is signed **ticks** at the given decimal scale (0–9), `Int64` LE (8 bytes) — same shape as `DateTime64`.

**Text format:** `[-]HH:MM:SS[.fraction]`. Unlike `DateTime` the hour field is *not* wrapped to a day — it is the total hour count and may exceed 23. The displayed value is **capped to ±999:59:59** (`3599999` seconds); a magnitude beyond the cap renders as `999:59:59` with a zeroed fraction (`999:59:59.000`). `CAST` also clamps the stored value to this range, but arithmetic can produce out-of-range values that are only clamped on display.

```
Time         value 45296            → 12:34:56     wire: f0 b0 00 00
Time64(3)    value 45296789 ticks   → 12:34:56.789 wire: 95 2c b3 02 00 00 00 00
```

> Requires `allow_experimental_time_time64_type = 1` on the server (these types are experimental as of v26.x).

#### 3.1.14 Interval

`Interval<Unit>` — `IntervalNanosecond`, `IntervalSecond`, `IntervalMinute`, `IntervalHour`, `IntervalDay`, `IntervalWeek`, `IntervalMonth`, `IntervalQuarter`, `IntervalYear`, etc. **Wire format:** the count as `Int64` LE (8 bytes). The unit lives **only in the type string** — it affects neither the wire encoding nor the text output, which is the bare integer (`5`). A single decoder path handles every unit.

#### 3.1.15 Geo types and SimpleAggregateFunction (type aliases)

Two families are pure **aliases** — the server sends the alias name in the column header, but the bytes on the wire are those of an underlying type, so a decoder maps the name to that type and reuses its codec:

| Type | Underlying wire type |
|------|----------------------|
| `Point` | `Tuple(Float64, Float64)` |
| `Ring`, `LineString` | `Array(Point)` |
| `Polygon`, `MultiLineString` | `Array(Ring)` |
| `MultiPolygon` | `Array(Polygon)` |
| `SimpleAggregateFunction(func, T[, …])` | its value type `T` |

Geo values therefore render as nested tuples/arrays (`(1,2)`, `[(0,0),(1,1)]`, …). `SimpleAggregateFunction` stores a *finalized* value, so its wire form and rendering are exactly those of `T`; only the single-value-type form is supported (multi-argument aggregate state types are not).

> `AggregateFunction(func, …)` (intermediate aggregation **state**, not a finalized value) and `QBit(T, N)` (bit-plane-transposed vector storage) are **not** decoded — their wire formats are function/codec specific. See the nested-types design note.

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

1. Every element stream encodes exactly `num_rows` values.
2. An empty column (`num_rows == 0`) writes zero bytes per stream.

**Empty tuple — `Tuple()`.** A zero-element tuple is legal (e.g. `SELECT tuple()`, `CAST(x AS Tuple())`). It has no element streams; instead it serializes like `Nothing` (§3.1.11) — **one placeholder byte (`0x30`, ASCII `'0'`) per row** — and the deserializer discards them. The row count comes from the block header. (Reference decoder: `parse_tuple_inner_types` returns an empty list and the decoder then consumes `num_rows` placeholder bytes.)

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

#### 3.4.5 Variant(T1, T2, ...)

A discriminated union: each row holds a value of exactly one of the variant types, or NULL. Every row carries a one-byte **global discriminator** selecting its type; the per-type values are then stored densely, one contiguous run per variant type.

**Type string syntax:** `Variant(T1, T2, ...)`. The server canonicalises the order (the variant types are sorted by name), so the type string as received already lists the types in **global-discriminator order**: discriminator `0` selects the first listed type, `1` the second, and so on. `255` (`NULL_DISCRIMINATOR`) means the row is NULL. Variant elements are never `Nullable` — NULL is the discriminator's job. Examples: `Variant(String, UInt64)`, `Variant(Array(UInt8), String)`.

**Discriminators mode.** The state prefix carries a `UInt64 LE` discriminators mode: `0` = BASIC (every row's discriminator written literally), `1` = COMPACT (run-length granule encoding). The server uses BASIC over the native protocol by default (`use_compact_variant_discriminators_serialization = false`); only BASIC is specified here.

**Wire layout (BASIC mode):**

```
[8 bytes:  UInt64 LE discriminators mode = 0]      ← state prefix, once per column per query
                                                     only emitted before the first block with rows > 0;
                                                     followed by each variant element's own state prefix
                                                     (empty for leaf types)
[per block with rows > 0]:
  [num_rows bytes: UInt8 discriminators]           ← one global discriminator per row; 255 = NULL
  [for each variant type i, in declared order]:
    [values for the rows whose discriminator == i] ← dense encoding in type i; count = #rows selecting i
```

**Reconstruction.** Walk the discriminators left to right, keeping a per-type running counter. Row `r` with discriminator `d` (≠ 255) takes the value at index `counter[d]` from variant type `d`'s value run, then `counter[d]` is incremented. Rows with discriminator `255` are NULL and consume no value from any run. The sum of the per-type counters equals the number of non-NULL rows.

**Invariants.**

1. State prefix (the mode `UInt64`) is read once per column per query, before the first block with rows > 0. Header/empty blocks emit nothing.
2. Each non-NULL discriminator is `<` the number of variant types.
3. Variant type `i` is decoded for exactly `count[i]` rows, where `count[i]` is the number of discriminators equal to `i`.

**Composes with.** Variant elements that are themselves stateful (`LowCardinality`, `Variant`, `Dynamic`, `JSON`) emit their own state prefix in the per-element state-prefix phase (after the mode `UInt64`); that nested case is not yet supported and is rejected with a clear error. Leaf types and the plain composites (`Array`, `Tuple`, `Map` of leaf types) have empty state prefixes and compose freely.

**Byte-level example — `Variant(String, UInt64)` with values `[42, 'hi', NULL]`** (canonical order sorts `String` before `UInt64`, so discriminator 0 = String, 1 = UInt64):

```
00 00 00 00 00 00 00 00      state prefix: UInt64 discriminators mode = 0 (BASIC)
01 00 FF                     discriminators (3 rows): 1 (UInt64), 0 (String), 255 (NULL)
02 68 69                     String run (1 value): len=2 "hi"
2A 00 00 00 00 00 00 00      UInt64 run (1 value): 42
```

Reconstructed: row 0 = UInt64 run[0] = `42`; row 1 = String run[0] = `"hi"`; row 2 = NULL.

#### 3.4.6 Dynamic

A column whose value type is discovered at runtime: each row holds a value of one of a runtime-determined set of types, or NULL. Unlike `Variant`, the type set is **not** in the column's type string — it is carried in the state prefix.

**Type string syntax:** `Dynamic` or `Dynamic(max_types=N)`. The `max_types` parameter bounds how many distinct types the column tracks but does not affect the wire format below.

**Serialization version.** `Dynamic` has several encodings (`V1=1`, `V2=2`, `FLATTENED=3`, `V3=4`). Over the native protocol the server emits **V2 by default**, which carries per-variant statistics. This spec (and the reference client) uses the simpler **FLATTENED (version 3)** encoding, which the client selects by sending the query setting `output_format_native_use_flattened_dynamic_and_json_serialization = 1`. Without that setting the server sends V2.

**Wire layout (FLATTENED, version 3):**

```
[8 bytes:  UInt64 LE version = 3]                  ← state prefix, once per column per query
                                                     only emitted before the first block with rows > 0
[VarUInt num_types]                                ← number of runtime types
[num_types × String]                               ← type names, in wire order
[per type: its own state prefix]                   ← empty for leaf types; + indexes-type prefix (empty, integer)
[per block with rows > 0]:
  [num_rows × discriminator]                       ← width by num_types (UInt8 if ≤ 255, else UInt16/32/64);
                                                     NULL discriminator = num_types (one past the last type)
  [for each type i, in wire order]:
    [values for the rows whose discriminator == i] ← dense encoding in type i
```

**Discriminator width** is the smallest unsigned integer that can index `num_types` types plus the NULL slot — `UInt8` for `num_types ≤ 255`, then `UInt16`, `UInt32`, `UInt64`. (Matches `getSmallestIndexesType(num_types + 1)`.)

**NULL** is the discriminator value `num_types` itself — note this differs from `Variant`, where NULL is the fixed value `255`.

**Reconstruction** is the same dense walk as `Variant`: keep a per-type counter, row `r` with discriminator `d` (≠ `num_types`) takes value `counter[d]` from type `d`'s run.

**Invariants.**

1. State prefix (version + type list) is read once per column per query, before the first block with rows > 0. Header/empty blocks emit nothing.
2. Runtime types whose serialization is stateful (`LowCardinality`/`Variant`/`Dynamic`/`JSON`) carry nested state prefixes after the type-name list; that nested case is not yet supported and is rejected.

**Byte-level example — `Dynamic` with runtime types `["UInt64", "String"]` and rows `[42, "hi", NULL]`** (discriminator 0 = UInt64, 1 = String, 2 = NULL):

```
03 00 00 00 00 00 00 00      state prefix: UInt64 version = 3 (FLATTENED)
02                           VarUInt num_types = 2
06 55 49 6E 74 36 34         type[0] = "UInt64"
06 53 74 72 69 6E 67         type[1] = "String"
00 01 02                     discriminators (3 rows): 0 (UInt64), 1 (String), 2 (NULL)
2A 00 00 00 00 00 00 00      UInt64 run (1 value): 42
02 68 69                     String run (1 value): len=2 "hi"
```

Reconstructed: row 0 = UInt64 run[0] = `42`; row 1 = String run[0] = `"hi"`; row 2 = NULL.

#### 3.4.7 JSON (Tier 2: FLATTENED Object)

The richer JSON encoding: instead of flattening every value to text (Tier 1, §3.4.4), the column is split into one sub-column per JSON path. The client selects it by **not** requesting the Tier 1 fallback (`output_format_native_write_json_as_string = 0`) while the flattened-serialization flag is on (`output_format_native_use_flattened_dynamic_and_json_serialization = 1`, which this client sets by default); the server then emits serialization **version 3**.

Two kinds of path:
- **Typed paths** are declared in the type string (`JSON(a UInt32, ``b.c`` String, ...)`) and decoded in their declared type.
- **Dynamic paths** are discovered at runtime and each decoded as a `Dynamic` column (§3.4.6).

In FLATTENED mode there is **no shared-data column** (that overflow store belongs to the non-flat V2/V3 Object encodings). Every path is a full column of `num_rows` values.

**Wire layout (version 3, FLATTENED):**

```
[8 bytes:  UInt64 LE version = 3]                  ← state prefix, once per column per query
[VarUInt num_dynamic_paths]
[num_dynamic_paths × String]                       ← dynamic path names, in wire order
[per typed path: its column's state prefix]        ← empty for leaf types
[per dynamic path: a Dynamic state prefix]         ← §3.4.6 (version + type list)
[per block with rows > 0]:
  [for each typed path:   its column's data]       ← num_rows values in the declared type
  [for each dynamic path: its Dynamic data]        ← num_rows values (§3.4.6 discriminators + runs)
```

Note the two-phase shape: **all** path state prefixes come first, then **all** path data. A dynamic path's `Dynamic` prefix (in the prefix phase) is therefore separated from its data (in the data phase).

**Invariants.**

1. State prefix read once per column per query, before the first block with rows > 0.
2. Every path column (typed or dynamic) holds exactly `num_rows` values.

**Reconstruction.** Row `r`'s object is assembled by reading each path's value at index `r`; a dynamic path whose `Dynamic` discriminator is NULL for that row contributes no key.

**Byte-level example — `JSON` value `{"a": 1, "b": "hi"}` (one row, both paths dynamic):**

```
03 00 00 00 00 00 00 00      version = 3 (Object)
02                           num_dynamic_paths = 2
01 61                        path "a"
01 62                        path "b"
03 00 00 00 00 00 00 00 01 06 55 49 6E 74 36 34   "a" Dynamic prefix: version 3, 1 type, "UInt64"
03 00 00 00 00 00 00 00 01 06 53 74 72 69 6E 67   "b" Dynamic prefix: version 3, 1 type, "String"
00 2A 00 00 00 00 00 00 00   "a" data: discriminator 0, UInt64 42
00 02 68 69                  "b" data: discriminator 0, String "hi"
```

#### 3.4.8 JSON non-flat (V2/V3) and Tier 3 — out of scope for this revision

The non-flattened `Object` encodings (V2/V3, used by MergeTree on-disk storage and emitted over the protocol when the flattened flag is off) carry a shared-data column and per-variant statistics. The reference client always requests either Tier 1 (String) or the FLATTENED Tier 2 form above, so those encodings are not specified here.

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

The compressed payload of a Block is a **stream of one or more frames**, not necessarily a single frame. The sender writes the serialized block through a compressed buffer that emits a frame whenever its internal buffer fills (≈1 MB) and a final frame when the block is flushed. So a small block is one frame; a large block is several consecutive frames; and the sender flushes at the end of each block, so a frame boundary always coincides with a block end.

A receiver streams the frames: read 16 + 9 bytes, read exactly `compressed_size - 9` body bytes, decompress to exactly `uncompressed_size` bytes, and serve those bytes to the block decoder; when the decoder needs more than the current frame holds, pull the next frame. Because the sender flushes per block, after a block is fully decoded the frame buffer is empty and the next block begins at a fresh frame.

The packet envelope — the packet-type VarUInt and the `table_name` string — is written to the **raw** stream, *outside* the compressed payload. Only the block body (BlockInfo + columns) is framed.

### 4.5 Negotiation

Compression is per-query, not per-connection. The protocol-level Query packet's `compression: bool` field requests it for that single query. The server honours the request and emits compressed Data/Totals/Extremes/Log/ProfileEvents bodies for the lifetime of the query (Log/ProfileEvents only at v54481+). It also expects the client's *outgoing* Data blocks — external tables, the empty end-of-data marker, and INSERT rows — to be framed the same way. Subsequent queries on the same connection may differ.

### 4.6 Reference client integration status

The reference Rust client wraps the stream with a `CompressedReader` (refills from frames on demand) and `CompressedWriter` (buffers a block, flushes one frame). The **read path is complete**: with `compression = true` the client decompresses all response block bodies, and compresses the client-side empty marker the server requires. **Compressed INSERT data (client→server) is deferred**: with compression on the server may also route columns through the parallel block-marshalling / `ColumnBLOB` path (v54478), which the flat decoder doesn't handle, so the client rejects compressed INSERT with a clear error rather than risk a desynchronised stream.

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
