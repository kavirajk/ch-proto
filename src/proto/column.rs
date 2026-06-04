use std::io::{Error, ErrorKind, Result};
use uuid::Uuid;

use super::{
    feature::Feature,
    wire::{ProtoRead, ProtoWrite},
};

// Column represents a single column in ClickHouse term.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub serialization: Serialization,
    pub data: ColumnData,
}

// Serialization describes the on-wire format used for this column's data.
// Feature-gated by CUSTOM_SERIALIZATION (v54454). External clients almost
// always produce Default; server may send non-Default for sparse /
// low-cardinality columns.
#[derive(Debug, Clone)]
pub enum Serialization {
    /// Default per-type serialization — values encoded directly, one per row.
    Default,
    /// Server-emitted non-default format (sparse, low-cardinality dictionary,
    /// etc.). The `kind_stack` is opaque metadata describing the variant.
    /// Decoding support is not yet implemented.
    Custom { kind_stack: Vec<u8> },
}

// ColumnData is in-memory representation of a single column data in ClickHouse terms
// Every value has single type.
#[derive(Debug, Clone)]
pub enum ColumnData {
    Uint8(Vec<u8>),
    Uint16(Vec<u16>),
    Uint32(Vec<u32>),
    Uint64(Vec<u64>),
    Int8(Vec<i8>),
    Int32(Vec<i32>),
    Int64(Vec<i64>),
    String(Vec<String>),
    /// FixedString(N): each value is exactly N bytes. Values shorter than N
    /// on INSERT are right-padded with NUL (`0x00`). We expose the raw bytes
    /// (Vec<u8> of length n * rows) rather than attempting UTF-8 decoding,
    /// because the bytes are not guaranteed to be valid UTF-8.
    FixedString {
        n: usize,
        data: Vec<u8>, // length = n * num_rows
    },
    // DateTime is UInt32 seconds-since-epoch on the wire.
    DateTime(Vec<u32>),
    Nullable {
        inner: Box<ColumnData>,
        nulls: Vec<u8>,
    },
    Array {
        inner: Box<ColumnData>,
        // NOTE(kavi): do we need u64 for offsets? come back later
        // probably because it's cumulative and sum can get wide range.
        // but still.
        offsets: Vec<u64>,
    },
    Tuple(Vec<ColumnData>),
    Int16(Vec<i16>),
    /// IEEE 754 single-precision, 4 bytes LE.
    Float32(Vec<f32>),
    /// IEEE 754 double-precision, 8 bytes LE.
    Float64(Vec<f64>),
    /// `Bool` is wire-compatible with `UInt8` (1 byte, 0/1) but the type
    /// string is literally `Bool` — kept as a distinct variant so the type
    /// string round-trips correctly.
    Bool(Vec<bool>),
    /// `Date`: UInt16 LE days since `1970-01-01`.
    Date(Vec<u16>),
    /// `Date32`: Int32 LE days since `1970-01-01`. Negative for dates
    /// before the epoch.
    Date32(Vec<i32>),
    /// `DateTime64(scale)` (or `DateTime64(scale, 'TZ')`): Int64 LE ticks
    /// at the given scale. Scale `0` = seconds, `3` = ms, `6` = µs, `9` = ns.
    /// Timezone (if any) is preserved on the type string but not in this
    /// in-memory shape — it affects display only, not the integer value.
    DateTime64 {
        scale: u8,
        values: Vec<i64>,
    },
    /// `UUID`: 16 bytes on the wire as **two byte-swapped LE UInt64 halves**.
    /// We expose canonical [`Uuid`] values; the byte-swap is applied at the
    /// encode/decode boundary (see SPEC §11.17).
    Uuid(Vec<Uuid>),
    /// `IPv4`: 4 bytes LE. The wire bytes are the canonical 32-bit IPv4
    /// integer in little-endian — i.e., the network-order bytes reversed.
    /// Stored as `u32` here; presentation as `a.b.c.d` is the caller's job.
    Ipv4(Vec<u32>),
    /// `IPv6`: 16 bytes verbatim in network byte order. Stored as `[u8; 16]`
    /// — same byte order as `std::net::Ipv6Addr::octets()`.
    Ipv6(Vec<[u8; 16]>),
    /// `Enum16` is wire-compatible with `Int16`. Variant labels live in the
    /// type string, byte layout is Int16 LE.
    Enum16(Vec<i16>),
    /// `Decimal(P, S)`. Width is implied by precision: P ≤ 9 → 4B, ≤ 18 → 8B,
    /// ≤ 38 → 16B, ≤ 76 → 32B. Stored here as the underlying signed integer
    /// in the matching width; scale is metadata for the caller.
    Decimal32 { scale: u8, values: Vec<i32> },
    Decimal64 { scale: u8, values: Vec<i64> },
    Decimal128 { scale: u8, values: Vec<i128> },
    /// 256-bit decimal — Rust has no `i256`, so we keep raw 32-byte LE
    /// two's-complement bytes. Sign interpretation is up to the caller.
    Decimal256 { scale: u8, values: Vec<[u8; 32]> },
    Int128(Vec<i128>),
    Uint128(Vec<u128>),
    /// 256-bit signed integer, raw 32-byte LE two's-complement bytes.
    /// Stored as raw bytes because Rust has no native `i256`.
    Int256(Vec<[u8; 32]>),
    /// 256-bit unsigned integer, raw 32-byte LE bytes.
    Uint256(Vec<[u8; 32]>),
    /// `LowCardinality(T)` — Tier 1 single-block-aware support.
    ///
    /// Wire format (per column):
    /// - State prefix (once per column per query, only when first block has
    ///   rows > 0): `Int64 LE = 1` (`sharedDictionariesWithAdditionalKeys`,
    ///   the only defined version).
    /// - Per block with rows > 0:
    ///   - `UInt64 LE` metadata: low byte = key type (0=UInt8, 1=UInt16,
    ///     2=UInt32, 3=UInt64); bit 9 = HasAdditionalKeysBit;
    ///     bit 10 = NeedUpdateDictionary; bit 11 = NeedGlobalDictionaryBit.
    ///   - `UInt64 LE` dict_size (typically includes a placeholder slot at
    ///     index 0 for the empty/null entry).
    ///   - dict values: `dict_size` values encoded as the inner type T.
    ///   - `UInt64 LE` keys_count = num_rows of this block.
    ///   - keys: `keys_count` indices, each `1 << key_type` bytes.
    ///
    /// Header blocks (rows == 0) are always empty — no state prefix or
    /// per-block data. KNOWN LIMITATION: this implementation handles
    /// queries with at most one data block per LowCardinality column.
    /// Multi-block queries would re-read the state prefix and fail.
    /// See SPEC §8.4.
    LowCardinality {
        dict: Box<ColumnData>,
        /// Keys promoted to u64 for ergonomics. The wire encoding uses the
        /// width specified by `key_width` (1, 2, 4, or 8 bytes).
        keys: Vec<u64>,
        /// 1, 2, 4, or 8 bytes per key. On encode, each key must fit in the
        /// chosen width.
        key_width: u8,
    },
    /// `JSON` — Tier 1 (String fallback) only.
    ///
    /// Wire format: `Int64 LE = 1` (JSONStringSerializationVersion) state
    /// prefix once at column start, then a regular String column encoding of
    /// `num_rows` JSON text values.
    ///
    /// The client auto-injects the `output_format_native_write_json_as_string`
    /// setting on every query so that JSON columns always come back in this
    /// shape. Tier 2 (FLATTENED, version 0/3/4) and the layered
    /// Variant/Dynamic infrastructure are not implemented; see SPEC §8.4.2.1.
    Json(Vec<String>),
    /// `Map(K, V)`: each row holds a variable-length sequence of key-value
    /// pairs. Wire format is identical to `Array(Tuple(K, V))`:
    ///   - `offsets` × num_rows UInt64 LE
    ///   - keys stream of `total_pairs` K values
    ///   - values stream of `total_pairs` V values
    /// where `total_pairs = offsets.last().unwrap_or(0)`.
    Map {
        keys: Box<ColumnData>,
        values: Box<ColumnData>,
        offsets: Vec<u64>,
    },
    /// `Nested(name1 T1, name2 T2, ...)` with `flatten_nested = 0` on the
    /// server. Wire format is byte-identical to `Array(Tuple(T1, T2, ...))`:
    ///   - `offsets` × num_rows UInt64 LE
    ///   - per-field stream of `total_elements` T_i values, in declaration order
    /// where `total_elements = offsets.last().unwrap_or(0)`.
    ///
    /// `fields` preserves the declared `(name, column)` pairs — the only
    /// metadata that distinguishes Nested from `Array(Tuple(...))` on the wire.
    ///
    /// Note: with default `flatten_nested = 1`, the server emits Nested as N
    /// separate `Array(T_i)` columns with dotted names (`n.a`, `n.b`) and this
    /// variant is never produced.
    Nested {
        fields: Vec<(String, ColumnData)>,
        offsets: Vec<u64>,
    },
    /// `Nothing` — a column type with no possible values. In practice it
    /// appears wrapped as `Nullable(Nothing)` for queries like `SELECT NULL`,
    /// where every value is NULL and the inner type carries no information.
    ///
    /// Wire format (matches `SerializationNothing::serializeBinaryBulk` in
    /// `ClickHouse/src/DataTypes/Serializations/SerializationNothing.cpp`):
    /// exactly **one placeholder byte per row** — the C++ side writes the
    /// ASCII character `'0'` (0x30); the deserializer discards the bytes
    /// without inspecting them. We follow the deserializer: read `rows` bytes
    /// and ignore them, store only the row count.
    Nothing(usize),
}

impl Column {
    pub fn encode(&self, w: &mut impl ProtoWrite, protocol: u32) -> Result<()> {
        w.write_string(&self.name)?;
        w.write_string(&self.data_type)?;

        if Feature::CUSTOM_SERIALIZATION.in_version(protocol) {
            match &self.serialization {
                Serialization::Default => w.write_u8(0)?,
                Serialization::Custom { kind_stack } => {
                    w.write_u8(1)?;
                    w.write_all(kind_stack)?;
                }
            }
        }

        self.data.encode(w)?;
        Ok(())
    }

    pub fn decode(r: &mut impl ProtoRead, rows: usize, protocol: u32) -> Result<Column> {
        let name = r.read_string()?;
        let data_type = r.read_string()?;

        let serialization = if Feature::CUSTOM_SERIALIZATION.in_version(protocol) {
            let has_custom = r.read_u8()?;
            match has_custom {
                0 => Serialization::Default,
                1 => {
                    // Per `SerializationInfo::deserializeFromKindsBinary`,
                    // the byte after `has_custom = 1` encodes the kind stack.
                    let kind_byte = r.read_u8()?;
                    Serialization::Custom {
                        kind_stack: vec![kind_byte],
                    }
                }
                other => {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("invalid has_custom_serialization byte: {other}"),
                    ));
                }
            }
        } else {
            Serialization::Default
        };

        let data = match &serialization {
            Serialization::Default => ColumnData::decode(r, &data_type, rows)?,
            Serialization::Custom { kind_stack } => {
                let kind = kind_stack[0];
                match kind {
                    KIND_DEFAULT => ColumnData::decode(r, &data_type, rows)?,
                    KIND_SPARSE => decode_sparse(r, &data_type, rows, name.as_str())?,
                    KIND_COMBINATION => {
                        // Multi-byte kind stack — we don't yet handle the
                        // composite forms (e.g. detached-over-sparse). For
                        // the differential harness we only see plain SPARSE
                        // in practice. Surface a clear error.
                        return Err(Error::new(
                            ErrorKind::Unsupported,
                            format!(
                                "column '{name}': COMBINATION kind stack not yet supported"
                            ),
                        ));
                    }
                    _ => {
                        return Err(Error::new(
                            ErrorKind::Unsupported,
                            format!(
                                "column '{name}': kind_stack byte {kind:#04x} not yet supported"
                            ),
                        ));
                    }
                }
            }
        };
        Ok(Column {
            name,
            data_type,
            serialization,
            data,
        })
    }
}

/// Byte values for the column-header kind_stack. Mirrors
/// `KindStackBinarySerializationType` in
/// `ClickHouse/src/DataTypes/Serializations/SerializationInfo.cpp`.
const KIND_DEFAULT: u8 = 0;
const KIND_SPARSE: u8 = 1;
const KIND_COMBINATION: u8 = 5;

/// Bit set on the trailing offset VarUInt of a sparse offsets stream to
/// mark "end of granule". Matches `END_OF_GRANULE_FLAG` in
/// `SerializationSparse.cpp`. The value (= 1 << 62) is the only EOG marker
/// we expect because the SerializationSparse VarUInt encoding caps values
/// at 2^63 anyway.
const END_OF_GRANULE_FLAG: u64 = 1 << 62;

/// Decode a sparse column. The wire format is:
///   - Offsets stream: VarUInts where each value is the count of default
///     positions before the next non-default. The trailing VarUInt has the
///     `END_OF_GRANULE_FLAG` bit set and encodes the count of trailing
///     defaults after the last non-default.
///   - Values stream: the non-default values, count = number of positions
///     decoded above, encoded in the inner type's dense form.
/// We materialize the result as a dense `ColumnData` of `rows` entries
/// because the rest of the client (TSV formatter, etc.) works on dense
/// columns. Default values are filled in at every non-explicit position.
fn decode_sparse(
    r: &mut impl ProtoRead,
    data_type: &str,
    rows: usize,
    name: &str,
) -> Result<ColumnData> {
    // Step 1: read offsets stream until we see the EOG terminator.
    let mut positions: Vec<usize> = Vec::new();
    let mut cursor = 0usize;
    loop {
        let raw = r.read_varuint()?;
        let group_size = (raw & !END_OF_GRANULE_FLAG) as usize;
        let is_eog = (raw & END_OF_GRANULE_FLAG) != 0;
        if is_eog {
            // `group_size` here is the trailing-defaults count.
            cursor += group_size;
            break;
        }
        let pos = cursor + group_size;
        if pos >= rows {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "column '{name}': sparse offset {pos} >= rows {rows}"
                ),
            ));
        }
        positions.push(pos);
        cursor = pos + 1;
    }
    if cursor != rows {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "column '{name}': sparse offsets cover {cursor} rows; expected {rows}"
            ),
        ));
    }

    // Step 2: read the non-default values densely.
    let values = ColumnData::decode(r, data_type, positions.len())?;

    // Step 3: materialize the dense column.
    materialize_sparse(values, &positions, rows, name)
}

/// Build a dense `ColumnData` of length `rows` from the sparse representation
/// (a values column of length `positions.len()` and the position list).
/// Default values are the type's zero — matches
/// `ColumnSparse::getDefaultValueAt` in C++ for the non-Nullable types we
/// currently support; Nullable inner type is rejected because nullable-sparse
/// composition is a separate v54483 feature (Problem 65).
fn materialize_sparse(
    values: ColumnData,
    positions: &[usize],
    rows: usize,
    name: &str,
) -> Result<ColumnData> {
    macro_rules! expand_vec {
        ($variant:ident, $inner:expr, $default:expr) => {{
            let mut dense = vec![$default; rows];
            for (i, &pos) in positions.iter().enumerate() {
                dense[pos] = $inner[i];
            }
            Ok(ColumnData::$variant(dense))
        }};
    }
    match values {
        ColumnData::Uint8(v) => expand_vec!(Uint8, v, 0u8),
        ColumnData::Uint16(v) => expand_vec!(Uint16, v, 0u16),
        ColumnData::Uint32(v) => expand_vec!(Uint32, v, 0u32),
        ColumnData::Uint64(v) => expand_vec!(Uint64, v, 0u64),
        ColumnData::Int8(v) => expand_vec!(Int8, v, 0i8),
        ColumnData::Int16(v) => expand_vec!(Int16, v, 0i16),
        ColumnData::Int32(v) => expand_vec!(Int32, v, 0i32),
        ColumnData::Int64(v) => expand_vec!(Int64, v, 0i64),
        ColumnData::Int128(v) => expand_vec!(Int128, v, 0i128),
        ColumnData::Uint128(v) => expand_vec!(Uint128, v, 0u128),
        ColumnData::Float32(v) => expand_vec!(Float32, v, 0.0f32),
        ColumnData::Float64(v) => expand_vec!(Float64, v, 0.0f64),
        ColumnData::Bool(v) => expand_vec!(Bool, v, false),
        ColumnData::Date(v) => expand_vec!(Date, v, 0u16),
        ColumnData::Date32(v) => expand_vec!(Date32, v, 0i32),
        ColumnData::DateTime(v) => expand_vec!(DateTime, v, 0u32),
        ColumnData::Enum16(v) => expand_vec!(Enum16, v, 0i16),
        ColumnData::Ipv4(v) => expand_vec!(Ipv4, v, 0u32),
        ColumnData::String(v) => {
            let mut dense = vec![String::new(); rows];
            for (i, value) in v.into_iter().enumerate() {
                dense[positions[i]] = value;
            }
            Ok(ColumnData::String(dense))
        }
        ColumnData::FixedString { n, data } => {
            // n bytes per row; defaults are n NUL bytes.
            let mut dense = vec![0u8; n.checked_mul(rows).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("column '{name}': FixedString size overflow n={n} rows={rows}"),
                )
            })?];
            for (i, &pos) in positions.iter().enumerate() {
                let src_start = i * n;
                let dst_start = pos * n;
                dense[dst_start..dst_start + n].copy_from_slice(&data[src_start..src_start + n]);
            }
            Ok(ColumnData::FixedString { n, data: dense })
        }
        ColumnData::Uuid(v) => {
            let mut dense = vec![uuid::Uuid::nil(); rows];
            for (i, val) in v.into_iter().enumerate() {
                dense[positions[i]] = val;
            }
            Ok(ColumnData::Uuid(dense))
        }
        ColumnData::Ipv6(v) => {
            let mut dense = vec![[0u8; 16]; rows];
            for (i, val) in v.into_iter().enumerate() {
                dense[positions[i]] = val;
            }
            Ok(ColumnData::Ipv6(dense))
        }
        ColumnData::DateTime64 { scale, values } => {
            let mut dense = vec![0i64; rows];
            for (i, val) in values.into_iter().enumerate() {
                dense[positions[i]] = val;
            }
            Ok(ColumnData::DateTime64 { scale, values: dense })
        }
        ColumnData::Decimal32 { scale, values } => {
            let mut dense = vec![0i32; rows];
            for (i, val) in values.into_iter().enumerate() {
                dense[positions[i]] = val;
            }
            Ok(ColumnData::Decimal32 { scale, values: dense })
        }
        ColumnData::Decimal64 { scale, values } => {
            let mut dense = vec![0i64; rows];
            for (i, val) in values.into_iter().enumerate() {
                dense[positions[i]] = val;
            }
            Ok(ColumnData::Decimal64 { scale, values: dense })
        }
        ColumnData::Decimal128 { scale, values } => {
            let mut dense = vec![0i128; rows];
            for (i, val) in values.into_iter().enumerate() {
                dense[positions[i]] = val;
            }
            Ok(ColumnData::Decimal128 { scale, values: dense })
        }
        ColumnData::Nullable { inner, nulls } => {
            // Sparse-over-Nullable (v54483+). The `values` column carries
            // both the inner T values AND their explicit null flags for
            // the non-default positions. At every non-explicit position the
            // default is "NULL" — i.e., the dense Nullable's null map gets
            // 1 there, and the dense inner gets whatever the inner type's
            // default is (irrelevant because nulls=1 hides it).
            let dense_inner = materialize_sparse(*inner, positions, rows, name)?;
            let mut dense_nulls = vec![1u8; rows];
            for (i, &pos) in positions.iter().enumerate() {
                dense_nulls[pos] = nulls[i];
            }
            Ok(ColumnData::Nullable {
                inner: Box::new(dense_inner),
                nulls: dense_nulls,
            })
        }
        other => Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "column '{name}': sparse decoder doesn't handle inner type {}",
                std::any::type_name_of_val(&other)
            ),
        )),
    }
}

impl ColumnData {
    /// The number of logical rows this column holds.
    ///
    /// For flat types, this is the length of the underlying `Vec`.
    /// For composites it is the **outer** row count: `Nullable` returns
    /// `nulls.len()`, `Array` returns `offsets.len()`. Inner ColumnData of a
    /// composite typically has a different row count (see §8.3 of the spec).
    pub fn row_count(&self) -> usize {
        match self {
            ColumnData::Uint8(v) => v.len(),
            ColumnData::Uint16(v) => v.len(),
            ColumnData::Uint32(v) => v.len(),
            ColumnData::Uint64(v) => v.len(),
            ColumnData::Int8(v) => v.len(),
            ColumnData::Int32(v) => v.len(),
            ColumnData::Int64(v) => v.len(),
            ColumnData::DateTime(v) => v.len(),
            ColumnData::String(v) => v.len(),
            ColumnData::FixedString { n, data } => {
                if *n == 0 {
                    0
                } else {
                    data.len() / n
                }
            }
            ColumnData::Nullable { nulls, .. } => nulls.len(),
            ColumnData::Array { offsets, .. } => offsets.len(),
            ColumnData::Tuple(v) => {
                // the invariant of all the row length of all the
                // types in v should be equal and that invariant is maintained
                // during apppend
                v.first().map_or(0, |d| d.row_count())
            }
            ColumnData::Map { offsets, .. } => offsets.len(),
            ColumnData::Nested { offsets, .. } => offsets.len(),
            ColumnData::Int16(v) => v.len(),
            ColumnData::Float32(v) => v.len(),
            ColumnData::Float64(v) => v.len(),
            ColumnData::Bool(v) => v.len(),
            ColumnData::Date(v) => v.len(),
            ColumnData::Date32(v) => v.len(),
            ColumnData::DateTime64 { values, .. } => values.len(),
            ColumnData::Uuid(v) => v.len(),
            ColumnData::Ipv4(v) => v.len(),
            ColumnData::Ipv6(v) => v.len(),
            ColumnData::Enum16(v) => v.len(),
            ColumnData::Decimal32 { values, .. } => values.len(),
            ColumnData::Decimal64 { values, .. } => values.len(),
            ColumnData::Decimal128 { values, .. } => values.len(),
            ColumnData::Decimal256 { values, .. } => values.len(),
            ColumnData::Int128(v) => v.len(),
            ColumnData::Uint128(v) => v.len(),
            ColumnData::Int256(v) => v.len(),
            ColumnData::Uint256(v) => v.len(),
            ColumnData::Json(v) => v.len(),
            ColumnData::LowCardinality { keys, .. } => keys.len(),
            ColumnData::Nothing(rows) => *rows,
        }
    }

    /// Validate internal consistency of a composite column before encoding.
    ///
    /// Flat types are always self-consistent by construction (their data is a
    /// single `Vec` whose length *is* the row count). Composites carry two
    /// pieces of length information that must match — this is where most
    /// programmer errors surface.
    ///
    /// Called at the top of `encode()` so that an inconsistent column is
    /// rejected before any bytes hit the wire.
    fn validate(&self) -> Result<()> {
        match self {
            ColumnData::Nullable { inner, nulls } => {
                if inner.row_count() != nulls.len() {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!(
                            "Nullable invariant broken: inner.row_count()={} != nulls.len()={}",
                            inner.row_count(),
                            nulls.len()
                        ),
                    ));
                }
                inner.validate()
            }
            ColumnData::Array { inner, offsets } => {
                // Offsets must be monotonic non-decreasing.
                for i in 1..offsets.len() {
                    if offsets[i] < offsets[i - 1] {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            format!(
                                "Array offsets not monotonic: offsets[{}]={} < offsets[{}]={}",
                                i,
                                offsets[i],
                                i - 1,
                                offsets[i - 1]
                            ),
                        ));
                    }
                }
                let total_elements = offsets.last().copied().unwrap_or(0) as usize;
                if inner.row_count() != total_elements {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!(
                            "Array invariant broken: inner.row_count()={} != offsets.last()={}",
                            inner.row_count(),
                            total_elements
                        ),
                    ));
                }
                inner.validate()
            }
            ColumnData::Tuple(v) => {
                // All inner element columns must agree on row count, and each
                // must itself be internally consistent.
                if let Some(first) = v.first() {
                    let expected = first.row_count();
                    for (i, inner) in v.iter().enumerate() {
                        if inner.row_count() != expected {
                            return Err(Error::new(
                                ErrorKind::InvalidInput,
                                format!(
                                    "Tuple invariant broken: element {i} row_count={} != element 0 row_count={expected}",
                                    inner.row_count()
                                ),
                            ));
                        }
                        inner.validate()?;
                    }
                }
                Ok(())
            }
            ColumnData::Map {
                keys,
                values,
                offsets,
            } => {
                // Same offset rules as Array: monotonic non-decreasing, and the
                // last offset is the total pair count that both inner streams
                // must match.
                for i in 1..offsets.len() {
                    if offsets[i] < offsets[i - 1] {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            format!(
                                "Map offsets not monotonic: offsets[{}]={} < offsets[{}]={}",
                                i,
                                offsets[i],
                                i - 1,
                                offsets[i - 1]
                            ),
                        ));
                    }
                }
                let total_pairs = offsets.last().copied().unwrap_or(0) as usize;
                if keys.row_count() != total_pairs {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!(
                            "Map invariant broken: keys.row_count()={} != offsets.last()={}",
                            keys.row_count(),
                            total_pairs
                        ),
                    ));
                }
                if values.row_count() != total_pairs {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!(
                            "Map invariant broken: values.row_count()={} != offsets.last()={}",
                            values.row_count(),
                            total_pairs
                        ),
                    ));
                }
                keys.validate()?;
                values.validate()?;
                Ok(())
            }
            ColumnData::Nested { fields, offsets } => {
                // Same offset rules as Array/Map: monotonic, last offset = total
                // elements that every per-field column must match.
                for i in 1..offsets.len() {
                    if offsets[i] < offsets[i - 1] {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            format!(
                                "Nested offsets not monotonic: offsets[{}]={} < offsets[{}]={}",
                                i,
                                offsets[i],
                                i - 1,
                                offsets[i - 1]
                            ),
                        ));
                    }
                }
                let total_elements = offsets.last().copied().unwrap_or(0) as usize;
                for (i, (name, col)) in fields.iter().enumerate() {
                    if col.row_count() != total_elements {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            format!(
                                "Nested invariant broken: field {i} '{name}' row_count={} != offsets.last()={}",
                                col.row_count(),
                                total_elements
                            ),
                        ));
                    }
                    col.validate()?;
                }
                Ok(())
            }
            ColumnData::FixedString { n, data } => {
                if *n == 0 {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "FixedString with n=0 is not allowed",
                    ));
                }
                if data.len() % n != 0 {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!(
                            "FixedString invariant broken: data.len()={} is not a multiple of n={}",
                            data.len(),
                            n
                        ),
                    ));
                }
                Ok(())
            }
            ColumnData::LowCardinality {
                dict,
                keys,
                key_width,
            } => {
                if !matches!(*key_width, 1 | 2 | 4 | 8) {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("LowCardinality key_width must be 1, 2, 4, or 8; got {key_width}"),
                    ));
                }
                let max_value: u64 = match *key_width {
                    1 => u8::MAX as u64,
                    2 => u16::MAX as u64,
                    4 => u32::MAX as u64,
                    _ => u64::MAX,
                };
                let dict_size = dict.row_count() as u64;
                for (i, &k) in keys.iter().enumerate() {
                    if k > max_value {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            format!(
                                "LowCardinality key {k} at index {i} doesn't fit in key_width={key_width}"
                            ),
                        ));
                    }
                    if k >= dict_size {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            format!(
                                "LowCardinality key {k} at index {i} >= dict_size {dict_size}"
                            ),
                        ));
                    }
                }
                dict.validate()
            }
            // Flat types: self-consistent by construction.
            _ => Ok(()),
        }
    }

    pub fn encode(&self, w: &mut impl ProtoWrite) -> Result<()> {
        self.validate()?;
        match self {
            ColumnData::Uint8(v) => {
                for &x in v {
                    w.write_u8(x)?;
                }
            }
            ColumnData::Uint16(v) => {
                for &x in v {
                    w.write_u16(x)?;
                }
            }
            ColumnData::Uint32(v) | ColumnData::DateTime(v) => {
                for &x in v {
                    w.write_u32(x)?;
                }
            }
            ColumnData::Uint64(v) => {
                for &x in v {
                    w.write_u64(x)?;
                }
            }
            ColumnData::Int8(v) => {
                for &x in v {
                    w.write_u8(x as u8)?;
                }
            }
            ColumnData::Int32(v) => {
                for &x in v {
                    w.write_i32(x)?;
                }
            }
            ColumnData::Int64(v) => {
                for &x in v {
                    w.write_i64(x)?;
                }
            }
            ColumnData::String(v) => {
                for s in v {
                    w.write_string(s)?;
                }
            }
            ColumnData::FixedString { n, data } => {
                let _ = n;
                w.write_all(data)?;
            }
            ColumnData::Nullable { inner, nulls } => {
                w.write_all(nulls)?;
                inner.encode(w)?;
            }
            ColumnData::Array { inner, offsets } => {
                for &off in offsets {
                    w.write_u64(off)?;
                }
                inner.encode(w)?;
            }
            ColumnData::Tuple(values) => {
                for v in values {
                    v.encode(w)?;
                }
            }
            ColumnData::Map {
                keys,
                values,
                offsets,
            } => {
                for &off in offsets {
                    w.write_u64(off)?;
                }
                keys.encode(w)?;
                values.encode(w)?;
            }
            ColumnData::Nested { fields, offsets } => {
                for &off in offsets {
                    w.write_u64(off)?;
                }
                for (_name, col) in fields {
                    col.encode(w)?;
                }
            }
            ColumnData::Int16(v) | ColumnData::Enum16(v) => {
                for &x in v {
                    w.write_i16(x)?;
                }
            }
            ColumnData::Float32(v) => {
                for &x in v {
                    w.write_f32(x)?;
                }
            }
            ColumnData::Float64(v) => {
                for &x in v {
                    w.write_f64(x)?;
                }
            }
            ColumnData::Bool(v) => {
                for &b in v {
                    w.write_bool(b)?;
                }
            }
            ColumnData::Date(v) => {
                for &x in v {
                    w.write_u16(x)?;
                }
            }
            ColumnData::Date32(v) => {
                for &x in v {
                    w.write_i32(x)?;
                }
            }
            ColumnData::DateTime64 { values, .. } => {
                for &x in values {
                    w.write_i64(x)?;
                }
            }
            ColumnData::Uuid(v) => {
                for u in v {
                    // Wire format: two byte-swapped LE UInt64 halves.
                    // canonical bytes 0..7 reversed → first 8 wire bytes;
                    // canonical bytes 8..15 reversed → next 8 wire bytes.
                    let bytes = u.as_bytes();
                    let mut hi = [0u8; 8];
                    hi.copy_from_slice(&bytes[..8]);
                    hi.reverse();
                    w.write_all(&hi)?;
                    let mut lo = [0u8; 8];
                    lo.copy_from_slice(&bytes[8..]);
                    lo.reverse();
                    w.write_all(&lo)?;
                }
            }
            ColumnData::Ipv4(v) => {
                for &x in v {
                    w.write_u32(x)?;
                }
            }
            ColumnData::Ipv6(v) => {
                for octets in v {
                    w.write_all(octets)?;
                }
            }
            ColumnData::Decimal32 { values, .. } => {
                for &x in values {
                    w.write_i32(x)?;
                }
            }
            ColumnData::Decimal64 { values, .. } => {
                for &x in values {
                    w.write_i64(x)?;
                }
            }
            ColumnData::Decimal128 { values, .. } => {
                for &x in values {
                    w.write_i128(x)?;
                }
            }
            ColumnData::Decimal256 { values, .. } => {
                for bytes in values {
                    w.write_all(bytes)?;
                }
            }
            ColumnData::Int128(v) => {
                for &x in v {
                    w.write_i128(x)?;
                }
            }
            ColumnData::Uint128(v) => {
                for &x in v {
                    w.write_u128(x)?;
                }
            }
            ColumnData::Int256(v) | ColumnData::Uint256(v) => {
                for bytes in v {
                    w.write_all(bytes)?;
                }
            }
            ColumnData::Json(v) => {
                // JSON Tier 1 wire layout: Int64 LE state prefix = 1, then
                // String column encoding of N rows.
                if !v.is_empty() {
                    w.write_i64(1)?;
                }
                for s in v {
                    w.write_string(s)?;
                }
            }
            ColumnData::LowCardinality {
                dict,
                keys,
                key_width,
            } => {
                // Skip everything for an empty column (header block).
                if keys.is_empty() {
                    // Nothing on the wire — matches the server's empty-block
                    // behaviour.
                } else {
                    // State prefix.
                    w.write_i64(1)?;
                    // Per-block metadata: key type in low byte + flags.
                    let key_type_code: u64 = match *key_width {
                        1 => 0,
                        2 => 1,
                        4 => 2,
                        8 => 3,
                        _ => 0, // validate() rejects other widths
                    };
                    let metadata: u64 = key_type_code | (1 << 9) | (1 << 10);
                    w.write_u64(metadata)?;
                    // Dict size.
                    w.write_u64(dict.row_count() as u64)?;
                    // Dict values.
                    dict.encode(w)?;
                    // Keys count.
                    w.write_u64(keys.len() as u64)?;
                    // Keys.
                    for &k in keys {
                        match *key_width {
                            1 => w.write_u8(k as u8)?,
                            2 => w.write_u16(k as u16)?,
                            4 => w.write_u32(k as u32)?,
                            8 => w.write_u64(k)?,
                            _ => unreachable!(),
                        }
                    }
                }
            }
            ColumnData::Nothing(rows) => {
                // Match SerializationNothing::serializeBinaryBulk: write the
                // ASCII character '0' (0x30) per row. Content is ignored on
                // decode; we keep the value the canonical server uses so the
                // bytes round-trip identically.
                for _ in 0..*rows {
                    w.write_u8(b'0')?;
                }
            }
        }
        Ok(())
    }

    pub fn decode(r: &mut impl ProtoRead, data_type: &str, rows: usize) -> Result<ColumnData> {
        // Strip parameterized types like "DateTime('UTC')" → "DateTime"
        let base_type = data_type.split('(').next().unwrap_or(data_type).trim();

        match base_type {
            "UInt8" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u8()?);
                }
                Ok(ColumnData::Uint8(v))
            }
            "UInt16" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u16()?);
                }
                Ok(ColumnData::Uint16(v))
            }
            "UInt32" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u32()?);
                }
                Ok(ColumnData::Uint32(v))
            }
            "UInt64" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u64()?);
                }
                Ok(ColumnData::Uint64(v))
            }
            // Enum8 is wire-compatible with Int8 (single byte per row).
            "Int8" | "Enum8" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u8()? as i8);
                }
                Ok(ColumnData::Int8(v))
            }
            // Enum16 is wire-compatible with Int16. We expose Enum16 as its
            // own variant so the type string round-trips, but the bytes are
            // identical to Int16. (See SPEC §11.8 / §11.18.)
            "Int16" | "Enum16" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_i16()?);
                }
                if base_type == "Enum16" {
                    Ok(ColumnData::Enum16(v))
                } else {
                    Ok(ColumnData::Int16(v))
                }
            }
            "Float32" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_f32()?);
                }
                Ok(ColumnData::Float32(v))
            }
            "Float64" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_f64()?);
                }
                Ok(ColumnData::Float64(v))
            }
            "Bool" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_bool()?);
                }
                Ok(ColumnData::Bool(v))
            }
            "Date" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u16()?);
                }
                Ok(ColumnData::Date(v))
            }
            "Date32" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_i32()?);
                }
                Ok(ColumnData::Date32(v))
            }
            "DateTime64" => {
                let scale = parse_datetime64_scale(data_type)?;
                let mut values = Vec::with_capacity(rows);
                for _ in 0..rows {
                    values.push(r.read_i64()?);
                }
                Ok(ColumnData::DateTime64 { scale, values })
            }
            "UUID" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    let mut hi = [0u8; 8];
                    r.read_exact(&mut hi)?;
                    hi.reverse();
                    let mut lo = [0u8; 8];
                    r.read_exact(&mut lo)?;
                    lo.reverse();
                    let mut bytes = [0u8; 16];
                    bytes[..8].copy_from_slice(&hi);
                    bytes[8..].copy_from_slice(&lo);
                    v.push(Uuid::from_bytes(bytes));
                }
                Ok(ColumnData::Uuid(v))
            }
            "IPv4" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u32()?);
                }
                Ok(ColumnData::Ipv4(v))
            }
            "IPv6" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    let mut bytes = [0u8; 16];
                    r.read_exact(&mut bytes)?;
                    v.push(bytes);
                }
                Ok(ColumnData::Ipv6(v))
            }
            "Decimal" => {
                // Decimal(P, S) — width is implied by P (see SPEC §8.1.x).
                let (precision, scale) = parse_decimal_p_s(data_type)?;
                match decimal_byte_width(precision)? {
                    4 => {
                        let mut values = Vec::with_capacity(rows);
                        for _ in 0..rows {
                            values.push(r.read_i32()?);
                        }
                        Ok(ColumnData::Decimal32 { scale, values })
                    }
                    8 => {
                        let mut values = Vec::with_capacity(rows);
                        for _ in 0..rows {
                            values.push(r.read_i64()?);
                        }
                        Ok(ColumnData::Decimal64 { scale, values })
                    }
                    16 => {
                        let mut values = Vec::with_capacity(rows);
                        for _ in 0..rows {
                            values.push(r.read_i128()?);
                        }
                        Ok(ColumnData::Decimal128 { scale, values })
                    }
                    32 => {
                        let mut values = Vec::with_capacity(rows);
                        for _ in 0..rows {
                            let mut bytes = [0u8; 32];
                            r.read_exact(&mut bytes)?;
                            values.push(bytes);
                        }
                        Ok(ColumnData::Decimal256 { scale, values })
                    }
                    _ => unreachable!("decimal_byte_width returns only 4/8/16/32"),
                }
            }
            "Int128" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_i128()?);
                }
                Ok(ColumnData::Int128(v))
            }
            "UInt128" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u128()?);
                }
                Ok(ColumnData::Uint128(v))
            }
            "Int256" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    let mut bytes = [0u8; 32];
                    r.read_exact(&mut bytes)?;
                    v.push(bytes);
                }
                Ok(ColumnData::Int256(v))
            }
            "UInt256" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    let mut bytes = [0u8; 32];
                    r.read_exact(&mut bytes)?;
                    v.push(bytes);
                }
                Ok(ColumnData::Uint256(v))
            }
            "LowCardinality" => {
                // Header block (rows == 0): server emits nothing; we mirror
                // that by returning an empty column with a placeholder dict.
                let inner_dt_full = parse_composite_inner_type(data_type)?;
                // For LC(Nullable(T)), the wire encodes the dict as plain T
                // — null is represented by convention as dict[1] (after the
                // empty placeholder at dict[0]). The Nullable wrapper has no
                // null-map stream of its own. Strip Nullable for the dict
                // decode; the in-memory shape preserves the LC structure
                // (callers who want null awareness check key == 1, or use
                // the original type string).
                let inner_dt = if inner_dt_full.starts_with("Nullable(") {
                    parse_composite_inner_type(&inner_dt_full)?
                } else {
                    inner_dt_full
                };
                if rows == 0 {
                    let dict = Box::new(ColumnData::decode(r, &inner_dt, 0)?);
                    return Ok(ColumnData::LowCardinality {
                        dict,
                        keys: Vec::new(),
                        key_width: 1,
                    });
                }
                // State prefix (Int64 LE = 1).
                let state_prefix = r.read_i64()?;
                if state_prefix != 1 {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        format!(
                            "LowCardinality state prefix {state_prefix} not supported \
                             (only `sharedDictionariesWithAdditionalKeys` = 1)"
                        ),
                    ));
                }
                // Per-block metadata.
                let metadata = r.read_u64()?;
                let key_type_code = (metadata & 0xFF) as u8;
                let key_width: u8 = match key_type_code {
                    0 => 1,
                    1 => 2,
                    2 => 4,
                    3 => 8,
                    other => {
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            format!("unknown LowCardinality key type code: {other}"),
                        ));
                    }
                };
                // Dict size + values.
                let dict_size = r.read_u64()? as usize;
                let dict = Box::new(ColumnData::decode(r, &inner_dt, dict_size)?);
                // Keys count + keys.
                let keys_count = r.read_u64()? as usize;
                let mut keys = Vec::with_capacity(keys_count);
                for _ in 0..keys_count {
                    let k: u64 = match key_width {
                        1 => r.read_u8()? as u64,
                        2 => r.read_u16()? as u64,
                        4 => r.read_u32()? as u64,
                        8 => r.read_u64()?,
                        _ => unreachable!(),
                    };
                    keys.push(k);
                }
                Ok(ColumnData::LowCardinality {
                    dict,
                    keys,
                    key_width,
                })
            }
            "JSON" => {
                // JSON Tier 1 (String fallback). The client auto-injects
                // `output_format_native_write_json_as_string=1`; the server
                // then emits an `Int64 LE = 1` state prefix followed by a
                // String column.
                //
                // KNOWN LIMITATION: the state prefix is per-column-per-query
                // (not per-block) and the server only emits it before the
                // first block with rows > 0. The header block (rows = 0) and
                // any subsequent blocks contain only the String values, no
                // prefix. We approximate with `rows == 0 → no prefix`, which
                // covers single-data-block queries (the common case) and the
                // header block correctly. Multi-block JSON queries are not
                // yet supported — see SPEC §8.4.
                if rows == 0 {
                    return Ok(ColumnData::Json(Vec::new()));
                }
                let version = r.read_i64()?;
                if version != 1 {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        format!(
                            "JSON serialization version {version} not supported \
                             (this client only handles version 1, the String fallback). \
                             Ensure `output_format_native_write_json_as_string=1` is set \
                             on the query — the client should inject it automatically."
                        ),
                    ));
                }
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_string()?);
                }
                Ok(ColumnData::Json(v))
            }
            "Int32" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_i32()?);
                }
                Ok(ColumnData::Int32(v))
            }
            "Int64" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_i64()?);
                }
                Ok(ColumnData::Int64(v))
            }
            "DateTime" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_u32()?);
                }
                Ok(ColumnData::DateTime(v))
            }
            "String" => {
                let mut v = Vec::with_capacity(rows);
                for _ in 0..rows {
                    v.push(r.read_string()?);
                }
                Ok(ColumnData::String(v))
            }
            "FixedString" => {
                let n = parse_fixed_string_n(data_type)?;
                let total = n.checked_mul(rows).ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("FixedString size overflow: n={n}, rows={rows}"),
                    )
                })?;
                let mut data = vec![0u8; total];
                r.read_exact(&mut data)?;
                Ok(ColumnData::FixedString { n, data })
            }
            "Nullable" => {
                let inner_data_type = parse_composite_inner_type(data_type)?;
                let mut nulls = vec![0u8; rows];
                r.read_exact(&mut nulls)?;
                let inner = Box::new(ColumnData::decode(r, &inner_data_type, rows)?);

                Ok(ColumnData::Nullable { inner, nulls })
            }
            "Nothing" => {
                // SerializationNothing writes one placeholder byte per row
                // (the ASCII character '0'). We follow the deserializer and
                // simply skip them — the only valid "value" of Nothing is the
                // absence of a value, which Nullable's null map already
                // signals. Common occurrence: `SELECT NULL` returns a
                // `Nullable(Nothing)` column where every entry is NULL.
                let mut placeholder = vec![0u8; rows];
                r.read_exact(&mut placeholder)?;
                Ok(ColumnData::Nothing(rows))
            }
            "Tuple" => {
                let inner_dts = parse_tuple_inner_types(data_type)?;
                let mut dts: Vec<ColumnData> = Vec::with_capacity(inner_dts.len());

                for dt in &inner_dts {
                    // each columnData is part of one element of Tuple(Vec<ColumnData>) for say
                    // Tuple(String, Int8, Int32). Each of those element has to have `row` number of
                    // values
                    let cd = ColumnData::decode(r, dt, rows)?;
                    dts.push(cd);
                }

                Ok(ColumnData::Tuple(dts))
            }

            "Array" => {
                let inner_dt = parse_composite_inner_type(data_type)?;
                let mut offsets: Vec<u64> = Vec::with_capacity(rows);
                for _ in 0..rows {
                    let off = r.read_u64()?;
                    offsets.push(off);
                }

                // Validate offsets are monotonic non-decreasing (SPEC §11.x,
                // see the offset semantics in §8.3.2). A corrupted or malicious
                // server that emits out-of-order offsets would cause downstream
                // garbage; fail loudly here.
                for i in 1..offsets.len() {
                    if offsets[i] < offsets[i - 1] {
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            format!(
                                "Array offsets not monotonic: offsets[{}]={} < offsets[{}]={}",
                                i,
                                offsets[i],
                                i - 1,
                                offsets[i - 1]
                            ),
                        ));
                    }
                }

                // Total element count = last cumulative offset (or 0 for empty column).
                let total_elements = offsets.last().copied().unwrap_or(0) as usize;

                let inner = Box::new(ColumnData::decode(r, &inner_dt, total_elements)?);

                Ok(ColumnData::Array { inner, offsets })
            }
            "Nested" => {
                // Wire format = Array(Tuple(T1, ..., Tn)). Parse field names
                // and types from the type string, read offsets, then decode
                // each field's stream sized to total_elements.
                let field_specs = parse_nested_inner_types(data_type)?;
                let mut offsets: Vec<u64> = Vec::with_capacity(rows);
                for _ in 0..rows {
                    offsets.push(r.read_u64()?);
                }
                for i in 1..offsets.len() {
                    if offsets[i] < offsets[i - 1] {
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            format!(
                                "Nested offsets not monotonic: offsets[{}]={} < offsets[{}]={}",
                                i,
                                offsets[i],
                                i - 1,
                                offsets[i - 1]
                            ),
                        ));
                    }
                }
                let total_elements = offsets.last().copied().unwrap_or(0) as usize;
                let mut fields: Vec<(String, ColumnData)> =
                    Vec::with_capacity(field_specs.len());
                for (name, dt) in field_specs {
                    let col = ColumnData::decode(r, &dt, total_elements)?;
                    fields.push((name, col));
                }
                Ok(ColumnData::Nested { fields, offsets })
            }
            "Map" => {
                // Wire format = Array(Tuple(K, V)). Parse out K, V from the
                // type string, read the offsets stream, then decode the keys
                // and values streams each sized to total_pairs.
                let (k_dt, v_dt) = parse_map_inner_types(data_type)?;
                let mut offsets: Vec<u64> = Vec::with_capacity(rows);
                for _ in 0..rows {
                    offsets.push(r.read_u64()?);
                }
                for i in 1..offsets.len() {
                    if offsets[i] < offsets[i - 1] {
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            format!(
                                "Map offsets not monotonic: offsets[{}]={} < offsets[{}]={}",
                                i,
                                offsets[i],
                                i - 1,
                                offsets[i - 1]
                            ),
                        ));
                    }
                }
                let total_pairs = offsets.last().copied().unwrap_or(0) as usize;
                let keys = Box::new(ColumnData::decode(r, &k_dt, total_pairs)?);
                let values = Box::new(ColumnData::decode(r, &v_dt, total_pairs)?);
                Ok(ColumnData::Map {
                    keys,
                    values,
                    offsets,
                })
            }
            _ => Err(Error::new(
                ErrorKind::Unsupported,
                format!("column type '{data_type}' not yet supported"),
            )),
        }
    }
}

/// Parse the field list of a `Nested(name1 T1, name2 T2, ...)` type string
/// into pairs of `(field_name, field_type)`. Each piece coming out of the
/// depth-aware splitter is `"name TYPE"` — split on the first ASCII
/// whitespace character. The type itself may contain spaces (e.g. inside
/// a nested `Tuple(a UInt8, b String)`), so we explicitly take only the
/// first whitespace as the separator.
///
/// Backtick-quoted identifiers (`` `field name` UInt8 ``) are not yet
/// supported; rare in practice for client-side INSERT/SELECT.
fn parse_nested_inner_types(data_type: &str) -> Result<Vec<(String, String)>> {
    let err = || {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid Nested type string: {data_type}"),
        )
    };
    let begin = data_type.find('(').ok_or_else(err)?;
    let end = data_type.rfind(')').ok_or_else(err)?;
    if begin + 1 >= end {
        return Err(err());
    }
    let inner = data_type[begin + 1..end].trim();
    let parts = split_with_composite(inner)?;

    let mut fields: Vec<(String, String)> = Vec::with_capacity(parts.len());
    for part in parts {
        let part = part.trim();
        let split = part
            .char_indices()
            .find(|(_, c)| c.is_ascii_whitespace())
            .map(|(i, _)| i);
        let Some(i) = split else {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Nested field missing type: '{part}' in {data_type}"),
            ));
        };
        let name = part[..i].trim().to_string();
        let ty = part[i + 1..].trim().to_string();
        if name.is_empty() || ty.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Nested field has empty name or type: '{part}' in {data_type}"),
            ));
        }
        fields.push((name, ty));
    }
    Ok(fields)
}

/// Parse the `K` and `V` from a `Map(K, V)` type string. Reuses the same
/// depth-aware splitter as Tuple — a Map is just a 2-tuple in its inner
/// shape.
fn parse_map_inner_types(data_type: &str) -> Result<(String, String)> {
    let err = || {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid Map type string: {data_type}"),
        )
    };
    let begin = data_type.find('(').ok_or_else(err)?;
    let end = data_type.rfind(')').ok_or_else(err)?;
    if begin + 1 >= end {
        return Err(err());
    }
    let inner = data_type[begin + 1..end].trim();
    let parts = split_with_composite(inner)?;
    if parts.len() != 2 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "Map requires exactly 2 type parameters, got {}: {data_type}",
                parts.len()
            ),
        ));
    }
    let mut iter = parts.into_iter();
    Ok((iter.next().unwrap(), iter.next().unwrap()))
}

// Pase the list of different types in Tuple(T1, T2,..). It's different than
// generic composite type with single inner type. Hence different helper.
// Tuple(Int8, String, Tuple(Int)) will return ["Int8", "String", "Tuple(Int)"].
fn parse_tuple_inner_types(data_type: &str) -> Result<Vec<String>> {
    let err = || {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid Tuple type string: {data_type}"),
        )
    };
    let begin = data_type.find('(').ok_or_else(err)?;
    let end = data_type.rfind(')').ok_or_else(err)?;
    if begin + 1 >= end {
        return Err(err());
    }

    let inner = data_type[begin + 1..end].trim().to_string();
    let dts: Vec<String> = split_with_composite(&inner)?;
    // Named-tuple support: `Tuple(name1 UInt64, name2 String)`. After the
    // depth-aware split each piece may carry a leading identifier; strip it
    // so the recursive decoder sees only the type. The wire format is the
    // same as for unnamed tuples — only the type string changes.
    Ok(dts.into_iter().map(|s| strip_field_name(&s).to_string()).collect())
}

// Drop a leading field name from a tuple element. Conservative rule: if
// the element has whitespace before its first `(` (or anywhere, when no
// parens), take everything from the last such whitespace forward. Handles:
//   "Int32"                  -> "Int32"
//   "name UInt64"            -> "UInt64"
//   "Tuple(Int8)"            -> "Tuple(Int8)"
//   "k Tuple(Int8, String)"  -> "Tuple(Int8, String)"
//   "n Nullable(UInt32)"     -> "Nullable(UInt32)"
fn strip_field_name(s: &str) -> &str {
    let s = s.trim();
    let cutoff = s.find('(').unwrap_or(s.len());
    let prefix = &s[..cutoff];
    if let Some(ws) = prefix.rfind(char::is_whitespace) {
        s[ws..].trim_start()
    } else {
        s
    }
}

// split the data type considering the composite nested depth.
// e.g
// "Int8, Float32, Int64" => ["Int8", "Float32", "Int64"]
// "Int8, Tuple(Int8, String), Int64" => ["Int8", "Tuple(Int8, String)", "Int64"]
fn split_with_composite(data_type: &str) -> Result<Vec<String>> {
    let mut depth = 0;
    let mut res: Vec<String> = Vec::new();
    let mut start = 0;
    let mut end = 0;
    for (i, c) in data_type.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' => {
                if depth == 0 && start < end {
                    res.push(data_type[start..=end].trim().to_string());
                    start = i + 1;
                }
            }
            _ => {}
        }
        end = i;
    }
    if depth != 0 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{data_type} string is invalid ClickHouse database"),
        ));
    }
    res.push(data_type[start..=end].trim().to_string());
    Ok(res)
}

// Parse the `T` in composite types like `Nullable(T)`, `Array(T)`
// from full type string.
// NOTE: T can be another ColumnData type. Hence the String return type.
fn parse_composite_inner_type(data_type: &str) -> Result<String> {
    let err = || {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid composite type string: {data_type}"),
        )
    };

    let open = data_type.find('(').ok_or_else(err)?;
    let close = data_type.rfind(')').ok_or_else(err)?;

    if open + 1 >= close {
        return Err(err());
    }

    Ok(data_type[open + 1..close].trim().to_string())
}

/// Parse the scale `N` from a `DateTime64(N)` or `DateTime64(N, 'TZ')` type
/// string. Scale is the first comma-separated parameter; it determines the
/// time unit (0 = seconds, 3 = ms, 6 = µs, 9 = ns).
fn parse_datetime64_scale(data_type: &str) -> Result<u8> {
    let err = || {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid DateTime64 type string: {data_type}"),
        )
    };
    let open = data_type.find('(').ok_or_else(err)?;
    let close = data_type.rfind(')').ok_or_else(err)?;
    if close <= open + 1 {
        return Err(err());
    }
    let inner = data_type[open + 1..close].trim();
    // Take the part before the first comma (timezone is optional).
    let scale_part = inner.split(',').next().unwrap_or(inner).trim();
    let n: u8 = scale_part.parse().map_err(|_| err())?;
    if n > 9 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("DateTime64 scale must be 0..=9, got {n}"),
        ));
    }
    Ok(n)
}

/// Parse `(P, S)` from a `Decimal(P, S)` type string.
fn parse_decimal_p_s(data_type: &str) -> Result<(u8, u8)> {
    let err = || {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid Decimal type string: {data_type}"),
        )
    };
    let open = data_type.find('(').ok_or_else(err)?;
    let close = data_type.rfind(')').ok_or_else(err)?;
    if close <= open + 1 {
        return Err(err());
    }
    let inner = data_type[open + 1..close].trim();
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if parts.len() != 2 {
        return Err(err());
    }
    let p: u8 = parts[0].parse().map_err(|_| err())?;
    let s: u8 = parts[1].parse().map_err(|_| err())?;
    Ok((p, s))
}

/// Map Decimal precision to underlying integer width in bytes.
/// Per ClickHouse: P ≤ 9 → 4B, ≤ 18 → 8B, ≤ 38 → 16B, ≤ 76 → 32B.
fn decimal_byte_width(precision: u8) -> Result<usize> {
    match precision {
        1..=9 => Ok(4),
        10..=18 => Ok(8),
        19..=38 => Ok(16),
        39..=76 => Ok(32),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Decimal precision must be 1..=76, got {precision}"),
        )),
    }
}

/// Parse the `N` in `FixedString(N)` from the full type string.
fn parse_fixed_string_n(data_type: &str) -> Result<usize> {
    let err = || {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid FixedString type string: {data_type}"),
        )
    };
    let open = data_type.find('(').ok_or_else(err)?;
    let close = data_type.rfind(')').ok_or_else(err)?;
    if close <= open + 1 {
        return Err(err());
    }
    data_type[open + 1..close]
        .trim()
        .parse::<usize>()
        .map_err(|_| err())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const PROTOCOL: u32 = 54459;
    const PROTOCOL_PRE_CUSTOM: u32 = 54453; // before CUSTOM_SERIALIZATION (54454)

    #[test]
    fn test_column_uint8_roundtrip() {
        let col = Column {
            name: "id".to_string(),
            data_type: "UInt8".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uint8(vec![1, 2, 3, 255, 0]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 5, PROTOCOL).unwrap();
        assert_eq!(decoded.name, "id");
        assert_eq!(decoded.data_type, "UInt8");
        match decoded.data {
            ColumnData::Uint8(v) => assert_eq!(v, vec![1, 2, 3, 255, 0]),
            _ => panic!("expected UInt8"),
        }
    }

    // -- Sparse serialization (v54465, Problem 50) --

    fn varuint_bytes(v: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        ProtoWrite::write_varuint(&mut buf, v).unwrap();
        buf
    }

    /// Hand-craft a sparse-encoded column body matching what the server would
    /// emit for an integer column of `rows` where `positions[i]` holds
    /// `values[i]` and all other positions are 0.
    fn build_sparse_uint32_wire(rows: usize, positions: &[usize], values: &[u32]) -> Vec<u8> {
        assert_eq!(positions.len(), values.len());
        let mut buf = Vec::new();
        // Offset stream: for each position, write (pos - cursor) varuint;
        // then write trailing-defaults | EOG_FLAG.
        let mut cursor = 0usize;
        for &pos in positions {
            buf.extend(varuint_bytes((pos - cursor) as u64));
            cursor = pos + 1;
        }
        let trailing = if cursor < rows { rows - cursor } else { 0 };
        buf.extend(varuint_bytes(trailing as u64 | END_OF_GRANULE_FLAG));
        // Values stream: dense little-endian UInt32 values.
        for &v in values {
            buf.extend(v.to_le_bytes());
        }
        buf
    }

    #[test]
    fn test_sparse_uint32_decode() {
        // 8 rows, non-default values at positions 2 and 6.
        let body = build_sparse_uint32_wire(8, &[2, 6], &[42, 99]);
        let mut cursor = Cursor::new(body.as_slice());
        let data = decode_sparse(&mut cursor, "UInt32", 8, "x").unwrap();
        match data {
            ColumnData::Uint32(v) => assert_eq!(v, vec![0, 0, 42, 0, 0, 0, 99, 0]),
            other => panic!("expected Uint32, got {:?}", other),
        }
    }

    #[test]
    fn test_sparse_all_defaults() {
        // EOG-only stream: every row is default (no positions).
        let mut body = Vec::new();
        body.extend(varuint_bytes(8 | END_OF_GRANULE_FLAG));
        // No values follow.
        let mut cursor = Cursor::new(body.as_slice());
        let data = decode_sparse(&mut cursor, "UInt32", 8, "x").unwrap();
        match data {
            ColumnData::Uint32(v) => assert_eq!(v, vec![0; 8]),
            other => panic!("expected Uint32, got {:?}", other),
        }
    }

    #[test]
    fn test_sparse_all_explicit() {
        // Three non-defaults, no trailing defaults.
        let body = build_sparse_uint32_wire(3, &[0, 1, 2], &[10, 20, 30]);
        let mut cursor = Cursor::new(body.as_slice());
        let data = decode_sparse(&mut cursor, "UInt32", 3, "x").unwrap();
        match data {
            ColumnData::Uint32(v) => assert_eq!(v, vec![10, 20, 30]),
            other => panic!("expected Uint32, got {:?}", other),
        }
    }

    #[test]
    fn test_sparse_string_decode() {
        // 5 rows, non-default "hi" at position 1, "world" at position 4.
        let mut body = Vec::new();
        // Offsets: position 1 (cursor 0 + 1 default), position 4 (cursor 2 + 2 defaults), EOG = 0 trailing.
        body.extend(varuint_bytes(1));
        body.extend(varuint_bytes(2));
        body.extend(varuint_bytes(0 | END_OF_GRANULE_FLAG));
        // Values: "hi", "world" (length-prefixed).
        body.extend(varuint_bytes(2));
        body.extend(b"hi");
        body.extend(varuint_bytes(5));
        body.extend(b"world");

        let mut cursor = Cursor::new(body.as_slice());
        let data = decode_sparse(&mut cursor, "String", 5, "x").unwrap();
        match data {
            ColumnData::String(v) => {
                assert_eq!(v, vec!["", "hi", "", "", "world"]);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_sparse_rejects_offset_past_rows() {
        // Position 10 in a 5-row column — invalid.
        let mut body = Vec::new();
        body.extend(varuint_bytes(10));
        body.extend(varuint_bytes(0 | END_OF_GRANULE_FLAG));
        let mut cursor = Cursor::new(body.as_slice());
        let err = decode_sparse(&mut cursor, "UInt32", 5, "x").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn test_sparse_over_nullable_decode() {
        // v54483 nullable-sparse: the values column is itself a Nullable
        // (null-map + inner values for the non-default positions); all
        // non-explicit positions in the dense output default to NULL.
        // Wire layout: [offsets][values_inner_dense as Nullable].
        // For 6 rows with non-default values at positions 1 and 4, where
        // both are non-null Int32 (e.g., 42 and 99):
        let mut body = Vec::new();
        // Offsets: pos 1 (cursor 0 + 1 default), pos 4 (cursor 2 + 2 defaults),
        // trailing 1 default (cursor 5..6).
        body.extend(varuint_bytes(1));
        body.extend(varuint_bytes(2));
        body.extend(varuint_bytes(1 | END_OF_GRANULE_FLAG));
        // Values stream is a Nullable(Int32) of length 2: null map then ints.
        body.push(0); // not null at i=0
        body.push(0); // not null at i=1
        body.extend(42i32.to_le_bytes());
        body.extend(99i32.to_le_bytes());

        let mut cursor = Cursor::new(body.as_slice());
        let data = decode_sparse(&mut cursor, "Nullable(Int32)", 6, "x").unwrap();
        match data {
            ColumnData::Nullable { inner, nulls } => {
                // Positions 0, 2, 3, 5 are NULL (default for Nullable).
                assert_eq!(nulls, vec![1, 0, 1, 1, 0, 1]);
                match *inner {
                    ColumnData::Int32(v) => {
                        // Inner at explicit positions holds the values;
                        // non-explicit positions hold defaults (0 here).
                        assert_eq!(v[1], 42);
                        assert_eq!(v[4], 99);
                    }
                    other => panic!("expected Int32 inner, got {:?}", other),
                }
            }
            other => panic!("expected Nullable, got {:?}", other),
        }
    }

    #[test]
    fn test_column_full_sparse_decode_via_header() {
        // Full Column::decode path: hand-craft a header (name, type, has_custom=1,
        // kind=0x01) plus a sparse body, then call Column::decode.
        let protocol = Feature::SPARSE_SERIALIZATION.version();
        let mut buf = Vec::new();
        ProtoWrite::write_string(&mut buf, "x").unwrap();
        ProtoWrite::write_string(&mut buf, "UInt32").unwrap();
        buf.push(1); // has_custom = 1
        buf.push(KIND_SPARSE); // kind = SPARSE (0x01)
        buf.extend(build_sparse_uint32_wire(8, &[2, 6], &[42, 99]));

        let mut cursor = Cursor::new(buf.as_slice());
        let col = Column::decode(&mut cursor, 8, protocol).unwrap();
        assert_eq!(col.name, "x");
        assert_eq!(col.data_type, "UInt32");
        match col.serialization {
            Serialization::Custom { kind_stack } => assert_eq!(kind_stack, vec![KIND_SPARSE]),
            other => panic!("expected Custom kind_stack, got {:?}", other),
        }
        match col.data {
            ColumnData::Uint32(v) => assert_eq!(v, vec![0, 0, 42, 0, 0, 0, 99, 0]),
            other => panic!("expected Uint32, got {:?}", other),
        }
    }

    #[test]
    fn test_column_nothing_roundtrip() {
        // `Nothing` columns appear in practice as the inner type of
        // `Nullable(Nothing)` — what `SELECT NULL` returns. The wire encoding
        // is exactly `rows` placeholder bytes (the canonical server writes
        // the ASCII character '0', 0x30); the deserializer ignores them.
        let col = Column {
            name: "n".to_string(),
            data_type: "Nothing".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nothing(4),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        // The four placeholder bytes must appear at the tail (header bytes
        // come first: name, type string, has_custom_serialization byte).
        assert_eq!(&buf[buf.len() - 4..], b"0000");

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 4, PROTOCOL).unwrap();
        assert_eq!(decoded.data_type, "Nothing");
        match decoded.data {
            ColumnData::Nothing(rows) => assert_eq!(rows, 4),
            _ => panic!("expected Nothing"),
        }
    }

    #[test]
    fn test_column_nullable_nothing_select_null() {
        // The shape `Nullable(Nothing)` is what `SELECT NULL` returns. The
        // null map is all 1s; the inner Nothing column is decoded but its
        // values are never read because Nullable's null bit short-circuits.
        let col = Column {
            name: "x".to_string(),
            data_type: "Nullable(Nothing)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nullable {
                inner: Box::new(ColumnData::Nothing(3)),
                nulls: vec![1, 1, 1],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nullable { inner, nulls } => {
                assert_eq!(nulls, vec![1, 1, 1]);
                match *inner {
                    ColumnData::Nothing(rows) => assert_eq!(rows, 3),
                    _ => panic!("expected Nothing inner"),
                }
            }
            _ => panic!("expected Nullable"),
        }
    }

    #[test]
    fn test_column_string_roundtrip() {
        let col = Column {
            name: "name".to_string(),
            data_type: "String".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::String(vec![
                "hello".to_string(),
                "".to_string(),
                "world".to_string(),
            ]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        assert_eq!(decoded.name, "name");
        match decoded.data {
            ColumnData::String(v) => assert_eq!(v, vec!["hello", "", "world"]),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn test_column_empty() {
        let col = Column {
            name: "x".to_string(),
            data_type: "UInt8".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uint8(vec![]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 0, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Uint8(v) => assert_eq!(v.len(), 0),
            _ => panic!("expected UInt8"),
        }
    }

    #[test]
    fn test_column_unsupported_type() {
        // Manually encode: name="x", type="Variant(UInt8, String)" (not yet
        // supported — versioned type with discriminator scheme, see SPEC
        // §8.4), has_custom=0
        let mut buf = Vec::new();
        buf.write_string("x").unwrap();
        buf.write_string("Variant(UInt8, String)").unwrap();
        buf.write_u8(0).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let err = Column::decode(&mut cursor, 0, PROTOCOL).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn test_column_unicode_values() {
        let col = Column {
            name: "名前".to_string(),
            data_type: "String".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::String(vec!["日本語".to_string(), "пароль".to_string()]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        assert_eq!(decoded.name, "名前");
        match decoded.data {
            ColumnData::String(v) => assert_eq!(v, vec!["日本語", "пароль"]),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn test_column_wire_includes_custom_serialization_byte() {
        // At PROTOCOL (>= 54454), the wire must include a `has_custom_serialization`
        // byte between the type string and the column data.
        let col = Column {
            name: "x".to_string(),
            data_type: "UInt8".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uint8(vec![]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        // Expected wire: [1 "x"] [5 "UInt8"] [0 (has_custom)]
        assert_eq!(
            buf,
            vec![0x01, b'x', 0x05, b'U', b'I', b'n', b't', b'8', 0x00]
        );
    }

    #[test]
    fn test_column_wire_omits_custom_serialization_byte_pre_feature() {
        // Before CUSTOM_SERIALIZATION (54454), the byte is NOT written.
        let col = Column {
            name: "x".to_string(),
            data_type: "UInt8".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uint8(vec![]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL_PRE_CUSTOM).unwrap();

        // Expected wire: [1 "x"] [5 "UInt8"] (no has_custom byte)
        assert_eq!(buf, vec![0x01, b'x', 0x05, b'U', b'I', b'n', b't', b'8']);
    }

    #[test]
    fn test_column_rejects_unsupported_kind_stack_on_decode() {
        // has_custom=1 + REPLICATED kind (0x04). Currently unsupported.
        let mut buf = Vec::new();
        buf.write_string("x").unwrap();
        buf.write_string("UInt8").unwrap();
        buf.write_u8(1).unwrap(); // has_custom
        buf.write_u8(4).unwrap(); // KIND_REPLICATED — not implemented

        let mut cursor = Cursor::new(buf.as_slice());
        let err = Column::decode(&mut cursor, 0, PROTOCOL).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    // -- String type (varuint-length + UTF-8) --

    #[test]
    fn test_string_empty_strings() {
        // All empty strings must still roundtrip (each is a single 0x00 byte on wire).
        let col = Column {
            name: "s".to_string(),
            data_type: "String".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::String(vec!["".to_string(), "".to_string(), "".to_string()]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::String(v) => {
                assert_eq!(v.len(), 3);
                for s in v {
                    assert_eq!(s, "");
                }
            }
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn test_string_with_embedded_nulls() {
        // Strings are byte sequences; embedded NUL is valid.
        let s = "a\0b\0c".to_string();
        let col = Column {
            name: "s".to_string(),
            data_type: "String".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::String(vec![s.clone()]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::String(v) => assert_eq!(v[0], s),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn test_string_large_values() {
        // Values that cross the VarUInt length-byte boundary (>= 128 bytes).
        let big = "x".repeat(500);
        let col = Column {
            name: "s".to_string(),
            data_type: "String".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::String(vec![big.clone(), "tiny".to_string(), big.clone()]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::String(v) => {
                assert_eq!(v[0].len(), 500);
                assert_eq!(v[1], "tiny");
                assert_eq!(v[2].len(), 500);
            }
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn test_string_wire_layout() {
        // Verify exact byte layout: each row = [VarUInt len][bytes]
        let col = Column {
            name: "s".to_string(),
            data_type: "String".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::String(vec!["ab".to_string(), "".to_string(), "c".to_string()]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        // [1 "s"] [6 "String"] [0 has_custom]
        // then: [2 'a' 'b'] [0] [1 'c']
        let expected = vec![
            0x01, b's', 0x06, b'S', b't', b'r', b'i', b'n', b'g',
            0x00, // has_custom_serialization
            0x02, b'a', b'b', 0x00, 0x01, b'c',
        ];
        assert_eq!(buf, expected);
    }

    // -- FixedString(N) --

    #[test]
    fn test_fixed_string_roundtrip() {
        // FixedString(4): each row is exactly 4 bytes.
        let data = b"abcd1234wxyz".to_vec(); // 3 rows of 4 bytes
        let col = Column {
            name: "s".to_string(),
            data_type: "FixedString(4)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::FixedString {
                n: 4,
                data: data.clone(),
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::FixedString { n, data: d } => {
                assert_eq!(n, 4);
                assert_eq!(d, data);
            }
            _ => panic!("expected FixedString"),
        }
    }

    #[test]
    fn test_fixed_string_parse_n() {
        assert_eq!(parse_fixed_string_n("FixedString(16)").unwrap(), 16);
        assert_eq!(parse_fixed_string_n("FixedString(1)").unwrap(), 1);
        assert_eq!(parse_fixed_string_n("FixedString( 42 )").unwrap(), 42);
    }

    #[test]
    fn test_fixed_string_parse_n_invalid() {
        assert!(parse_fixed_string_n("FixedString").is_err());
        assert!(parse_fixed_string_n("FixedString()").is_err());
        assert!(parse_fixed_string_n("FixedString(abc)").is_err());
        assert!(parse_fixed_string_n("FixedString(16").is_err());
    }

    #[test]
    fn test_fixed_string_with_null_padding() {
        // Server right-pads short values with NUL. Roundtrip preserves those bytes.
        let data = vec![b'h', b'i', 0, 0, 0, b'x', 0, 0, 0, 0]; // 2 rows of 5 bytes
        let col = Column {
            name: "s".to_string(),
            data_type: "FixedString(5)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::FixedString {
                n: 5,
                data: data.clone(),
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::FixedString { n, data: d } => {
                assert_eq!(n, 5);
                assert_eq!(d, data);
            }
            _ => panic!("expected FixedString"),
        }
    }

    #[test]
    fn test_fixed_string_zero_rows() {
        let col = Column {
            name: "s".to_string(),
            data_type: "FixedString(8)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::FixedString { n: 8, data: vec![] },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 0, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::FixedString { n, data: d } => {
                assert_eq!(n, 8);
                assert!(d.is_empty());
            }
            _ => panic!("expected FixedString"),
        }
    }

    #[test]
    fn test_fixed_string_wire_layout() {
        // No length prefix per value — just concatenated bytes.
        let col = Column {
            name: "s".to_string(),
            data_type: "FixedString(3)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::FixedString {
                n: 3,
                data: vec![b'a', b'b', b'c', b'd', b'e', b'f'],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        // [1 "s"] [14 "FixedString(3)"] [0 has_custom] [6 bytes of data]
        let expected = vec![
            0x01, b's', 0x0E, b'F', b'i', b'x', b'e', b'd', b'S', b't', b'r', b'i', b'n', b'g',
            b'(', b'3', b')', 0x00, b'a', b'b', b'c', b'd', b'e', b'f',
        ];
        assert_eq!(buf, expected);
    }

    #[test]
    fn test_fixed_string_non_utf8_bytes() {
        // FixedString holds raw bytes; arbitrary non-UTF-8 must roundtrip.
        let data = vec![0xFF, 0xFE, 0x00, 0x80, 0xC0, 0xC1]; // 2 rows of 3 bytes, invalid UTF-8
        let col = Column {
            name: "s".to_string(),
            data_type: "FixedString(3)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::FixedString {
                n: 3,
                data: data.clone(),
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::FixedString { data: d, .. } => assert_eq!(d, data),
            _ => panic!("expected FixedString"),
        }
    }

    // -- Nullable(T) --

    #[test]
    fn test_nullable_uint32_roundtrip() {
        // Nullable(UInt32) with [10, null, 20] — placeholder byte for null row is 0.
        let col = Column {
            name: "x".to_string(),
            data_type: "Nullable(UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nullable {
                inner: Box::new(ColumnData::Uint32(vec![10, 0, 20])),
                nulls: vec![0, 1, 0],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nullable { inner, nulls } => {
                assert_eq!(nulls, vec![0, 1, 0]);
                match *inner {
                    ColumnData::Uint32(v) => assert_eq!(v, vec![10, 0, 20]),
                    _ => panic!("expected Uint32 inner"),
                }
            }
            _ => panic!("expected Nullable"),
        }
    }

    #[test]
    fn test_nullable_all_nulls() {
        let col = Column {
            name: "x".to_string(),
            data_type: "Nullable(UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nullable {
                inner: Box::new(ColumnData::Uint32(vec![0, 0, 0])),
                nulls: vec![1, 1, 1],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nullable { nulls, .. } => assert_eq!(nulls, vec![1, 1, 1]),
            _ => panic!("expected Nullable"),
        }
    }

    #[test]
    fn test_nullable_no_nulls() {
        let col = Column {
            name: "x".to_string(),
            data_type: "Nullable(UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nullable {
                inner: Box::new(ColumnData::Uint32(vec![1, 2, 3])),
                nulls: vec![0, 0, 0],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nullable { inner, nulls } => {
                assert_eq!(nulls, vec![0, 0, 0]);
                match *inner {
                    ColumnData::Uint32(v) => assert_eq!(v, vec![1, 2, 3]),
                    _ => panic!("expected Uint32 inner"),
                }
            }
            _ => panic!("expected Nullable"),
        }
    }

    #[test]
    fn test_nullable_zero_rows() {
        let col = Column {
            name: "x".to_string(),
            data_type: "Nullable(UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nullable {
                inner: Box::new(ColumnData::Uint32(vec![])),
                nulls: vec![],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 0, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nullable { inner, nulls } => {
                assert!(nulls.is_empty());
                match *inner {
                    ColumnData::Uint32(v) => assert!(v.is_empty()),
                    _ => panic!("expected Uint32 inner"),
                }
            }
            _ => panic!("expected Nullable"),
        }
    }

    #[test]
    fn test_nullable_string_roundtrip() {
        // Placeholder for null row is an empty string (wire: single 0x00 byte).
        let col = Column {
            name: "s".to_string(),
            data_type: "Nullable(String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nullable {
                inner: Box::new(ColumnData::String(vec![
                    "hello".to_string(),
                    "".to_string(),
                    "world".to_string(),
                ])),
                nulls: vec![0, 1, 0],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nullable { inner, nulls } => {
                assert_eq!(nulls, vec![0, 1, 0]);
                match *inner {
                    ColumnData::String(v) => assert_eq!(v, vec!["hello", "", "world"]),
                    _ => panic!("expected String inner"),
                }
            }
            _ => panic!("expected Nullable"),
        }
    }

    #[test]
    fn test_nullable_wire_layout() {
        // Verify exact wire bytes: null-map first, then inner.
        // Nullable(UInt8) with [5, null, 9]
        let col = Column {
            name: "x".to_string(),
            data_type: "Nullable(UInt8)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nullable {
                inner: Box::new(ColumnData::Uint8(vec![5, 0, 9])),
                nulls: vec![0, 1, 0],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        // [1 "x"] [15 "Nullable(UInt8)"] [0 has_custom] [0 1 0 null-map] [5 0 9 inner]
        let expected = vec![
            0x01, b'x', // name
            0x0F, b'N', b'u', b'l', b'l', b'a', b'b', b'l', b'e', b'(', b'U', // type
            b'I', b'n', b't', b'8', b')', //
            0x00, // has_custom_serialization
            0x00, 0x01, 0x00, // null map (present, null, present)
            0x05, 0x00, 0x09, // inner UInt8 values
        ];
        assert_eq!(buf, expected);
    }

    #[test]
    fn test_nullable_wire_order_is_null_map_first() {
        // Regression guard: encode must put null-map BEFORE inner values.
        // A swapped encoder would produce [0x05, 0x09 ...] before [0x00, 0x01, ...].
        let col = Column {
            name: "x".to_string(),
            data_type: "Nullable(UInt8)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nullable {
                inner: Box::new(ColumnData::Uint8(vec![0xAA, 0xBB])),
                nulls: vec![0xFF, 0x00],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        // The last 4 bytes of buf should be: null-map (0xFF, 0x00), then inner (0xAA, 0xBB).
        let tail = &buf[buf.len() - 4..];
        assert_eq!(tail, &[0xFF, 0x00, 0xAA, 0xBB]);
    }

    #[test]
    fn test_parse_composite_inner_type() {
        assert_eq!(
            parse_composite_inner_type("Nullable(UInt32)").unwrap(),
            "UInt32"
        );
        assert_eq!(
            parse_composite_inner_type("Nullable(DateTime('UTC'))").unwrap(),
            "DateTime('UTC')"
        );
        // Nested — rfind(')') correctly takes the outermost.
        assert_eq!(
            parse_composite_inner_type("Nullable(FixedString(16))").unwrap(),
            "FixedString(16)"
        );
    }

    #[test]
    fn test_parse_composite_inner_type_invalid() {
        assert!(parse_composite_inner_type("Nullable").is_err());
        assert!(parse_composite_inner_type("Nullable()").is_err());
        assert!(parse_composite_inner_type("Nullable(UInt32").is_err());
    }

    #[test]
    fn test_nullable_fixed_string_roundtrip() {
        // Compose Nullable with a parameterized inner type.
        // FixedString(3) with 3 rows: "abc", <null placeholder>, "xyz"
        let data = vec![b'a', b'b', b'c', 0, 0, 0, b'x', b'y', b'z'];
        let col = Column {
            name: "f".to_string(),
            data_type: "Nullable(FixedString(3))".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nullable {
                inner: Box::new(ColumnData::FixedString {
                    n: 3,
                    data: data.clone(),
                }),
                nulls: vec![0, 1, 0],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nullable { inner, nulls } => {
                assert_eq!(nulls, vec![0, 1, 0]);
                match *inner {
                    ColumnData::FixedString { n, data: d } => {
                        assert_eq!(n, 3);
                        assert_eq!(d, data);
                    }
                    _ => panic!("expected FixedString inner"),
                }
            }
            _ => panic!("expected Nullable"),
        }
    }

    // -- Array(T) --

    #[test]
    fn test_array_uint32_roundtrip() {
        // Array(UInt32) with 3 rows: [[10, 20, 30], [], [40, 50]]
        let col = Column {
            name: "arr".to_string(),
            data_type: "Array(UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Array {
                inner: Box::new(ColumnData::Uint32(vec![10, 20, 30, 40, 50])),
                offsets: vec![3, 3, 5],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Array { inner, offsets } => {
                assert_eq!(offsets, vec![3, 3, 5]);
                match *inner {
                    ColumnData::Uint32(v) => assert_eq!(v, vec![10, 20, 30, 40, 50]),
                    _ => panic!("expected Uint32 inner"),
                }
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_array_all_empty() {
        // 3 rows, each an empty array.
        let col = Column {
            name: "arr".to_string(),
            data_type: "Array(UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Array {
                inner: Box::new(ColumnData::Uint32(vec![])),
                offsets: vec![0, 0, 0],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Array { inner, offsets } => {
                assert_eq!(offsets, vec![0, 0, 0]);
                match *inner {
                    ColumnData::Uint32(v) => assert!(v.is_empty()),
                    _ => panic!("expected Uint32 inner"),
                }
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_array_zero_rows() {
        // 0 rows: no offsets, no inner values.
        let col = Column {
            name: "arr".to_string(),
            data_type: "Array(UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Array {
                inner: Box::new(ColumnData::Uint32(vec![])),
                offsets: vec![],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 0, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Array { inner, offsets } => {
                assert!(offsets.is_empty());
                match *inner {
                    ColumnData::Uint32(v) => assert!(v.is_empty()),
                    _ => panic!("expected Uint32 inner"),
                }
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_array_string_roundtrip() {
        // Array(String) with [["a", "bb"], [], ["c"]]
        let col = Column {
            name: "arr".to_string(),
            data_type: "Array(String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Array {
                inner: Box::new(ColumnData::String(vec![
                    "a".to_string(),
                    "bb".to_string(),
                    "c".to_string(),
                ])),
                offsets: vec![2, 2, 3],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Array { inner, offsets } => {
                assert_eq!(offsets, vec![2, 2, 3]);
                match *inner {
                    ColumnData::String(v) => assert_eq!(v, vec!["a", "bb", "c"]),
                    _ => panic!("expected String inner"),
                }
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_array_nested_array_array_uint32() {
        // Array(Array(UInt32)) with 3 rows: [[[1,2]], [], [[3], [4,5]]]
        // Outer offsets = [1, 1, 3]           (inner count per outer row: 1, 0, 2)
        // Middle Array(UInt32) row count = 3 (one per inner-array)
        // Middle offsets = [2, 3, 5]
        // Innermost values = [1, 2, 3, 4, 5]
        let col = Column {
            name: "arr".to_string(),
            data_type: "Array(Array(UInt32))".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Array {
                inner: Box::new(ColumnData::Array {
                    inner: Box::new(ColumnData::Uint32(vec![1, 2, 3, 4, 5])),
                    offsets: vec![2, 3, 5],
                }),
                offsets: vec![1, 1, 3],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Array {
                inner,
                offsets: outer_offsets,
            } => {
                assert_eq!(outer_offsets, vec![1, 1, 3]);
                match *inner {
                    ColumnData::Array {
                        inner: inner2,
                        offsets: mid_offsets,
                    } => {
                        assert_eq!(mid_offsets, vec![2, 3, 5]);
                        match *inner2 {
                            ColumnData::Uint32(v) => assert_eq!(v, vec![1, 2, 3, 4, 5]),
                            _ => panic!("expected Uint32 innermost"),
                        }
                    }
                    _ => panic!("expected Array middle layer"),
                }
            }
            _ => panic!("expected outer Array"),
        }
    }

    #[test]
    fn test_array_nullable_inner() {
        // Array(Nullable(UInt32)) with [[1, null, 3], [null]]
        // Outer offsets = [3, 4]
        // Inner Nullable(UInt32) row count = 4
        let col = Column {
            name: "arr".to_string(),
            data_type: "Array(Nullable(UInt32))".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Array {
                inner: Box::new(ColumnData::Nullable {
                    inner: Box::new(ColumnData::Uint32(vec![1, 0, 3, 0])),
                    nulls: vec![0, 1, 0, 1],
                }),
                offsets: vec![3, 4],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Array { inner, offsets } => {
                assert_eq!(offsets, vec![3, 4]);
                match *inner {
                    ColumnData::Nullable { nulls, .. } => {
                        assert_eq!(nulls, vec![0, 1, 0, 1]);
                    }
                    _ => panic!("expected Nullable inner"),
                }
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_array_wire_layout() {
        // Verify byte layout: offsets (UInt64 LE) first, then inner values.
        // Array(UInt8) with [[5, 10], [20]]: offsets=[2,3], values=[5,10,20]
        let col = Column {
            name: "a".to_string(),
            data_type: "Array(UInt8)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Array {
                inner: Box::new(ColumnData::Uint8(vec![5, 10, 20])),
                offsets: vec![2, 3],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        // Expected tail (after header): 2 offsets × 8 bytes, then 3 UInt8 values.
        let expected_tail = vec![
            0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // offsets[0] = 2
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // offsets[1] = 3
            0x05, 0x0A, 0x14, // UInt8 values: 5, 10, 20
        ];
        let tail = &buf[buf.len() - expected_tail.len()..];
        assert_eq!(tail, expected_tail.as_slice());
    }

    // -- row_count() helper --

    #[test]
    fn test_row_count_flat_types() {
        assert_eq!(ColumnData::Uint8(vec![1, 2, 3]).row_count(), 3);
        assert_eq!(ColumnData::String(vec!["a".to_string()]).row_count(), 1);
        assert_eq!(ColumnData::Uint64(vec![]).row_count(), 0);
    }

    #[test]
    fn test_row_count_fixed_string() {
        let c = ColumnData::FixedString {
            n: 4,
            data: vec![0; 12],
        };
        assert_eq!(c.row_count(), 3);
    }

    #[test]
    fn test_row_count_nullable() {
        // Nullable row count is nulls.len(), regardless of inner.
        let c = ColumnData::Nullable {
            inner: Box::new(ColumnData::Uint8(vec![1, 2, 3])),
            nulls: vec![0, 1, 0],
        };
        assert_eq!(c.row_count(), 3);
    }

    #[test]
    fn test_row_count_array() {
        // Array row count is offsets.len(), NOT inner.row_count().
        let c = ColumnData::Array {
            inner: Box::new(ColumnData::Uint32(vec![1, 2, 3, 4, 5])), // 5 inner values
            offsets: vec![2, 3, 5],                                   // 3 outer rows
        };
        assert_eq!(c.row_count(), 3);
    }

    // -- validate() — catches encode-time invariant violations --

    #[test]
    fn test_validate_rejects_mismatched_nullable() {
        // 3 nulls but 2 inner values — invalid.
        let c = ColumnData::Nullable {
            inner: Box::new(ColumnData::Uint32(vec![1, 2])),
            nulls: vec![0, 1, 0],
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_rejects_mismatched_array() {
        // offsets.last() = 5 but inner has only 3 values — invalid.
        let c = ColumnData::Array {
            inner: Box::new(ColumnData::Uint32(vec![1, 2, 3])),
            offsets: vec![2, 3, 5],
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_rejects_non_monotonic_offsets() {
        // offsets[2]=1 < offsets[1]=3 — invalid.
        let c = ColumnData::Array {
            inner: Box::new(ColumnData::Uint32(vec![1, 2, 3, 4])),
            offsets: vec![2, 3, 1],
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_rejects_fixed_string_with_bad_length() {
        // n=4, but data has 7 bytes (not a multiple of 4).
        let c = ColumnData::FixedString {
            n: 4,
            data: vec![0; 7],
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_rejects_decode_with_non_monotonic_offsets() {
        // Hand-craft a wire stream with a bad Array and verify decode catches it.
        let mut buf = Vec::new();
        buf.write_string("a").unwrap();
        buf.write_string("Array(UInt8)").unwrap();
        buf.write_u8(0).unwrap(); // has_custom_serialization
        buf.write_u64(3).unwrap();
        buf.write_u64(1).unwrap(); // <- decreases from 3 to 1 (invalid)

        let mut cursor = Cursor::new(buf.as_slice());
        let err = Column::decode(&mut cursor, 2, PROTOCOL).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn test_validate_accepts_valid_composite() {
        let c = ColumnData::Array {
            inner: Box::new(ColumnData::Uint32(vec![1, 2, 3, 4, 5])),
            offsets: vec![3, 3, 5],
        };
        let mut buf = Vec::new();
        c.encode(&mut buf).unwrap();
        // No panic, no error.
    }

    // -- Tuple(...) --

    #[test]
    fn test_tuple_uint32_string_roundtrip() {
        // Tuple(UInt32, String) with 3 rows: (10, "a"), (20, "bb"), (30, "ccc")
        let col = Column {
            name: "t".to_string(),
            data_type: "Tuple(UInt32, String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Tuple(vec![
                ColumnData::Uint32(vec![10, 20, 30]),
                ColumnData::String(vec!["a".to_string(), "bb".to_string(), "ccc".to_string()]),
            ]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                match &elems[0] {
                    ColumnData::Uint32(v) => assert_eq!(v, &vec![10, 20, 30]),
                    _ => panic!("expected Uint32 element 0"),
                }
                match &elems[1] {
                    ColumnData::String(v) => assert_eq!(v, &vec!["a", "bb", "ccc"]),
                    _ => panic!("expected String element 1"),
                }
            }
            _ => panic!("expected Tuple"),
        }
    }

    #[test]
    fn test_tuple_zero_rows() {
        let col = Column {
            name: "t".to_string(),
            data_type: "Tuple(UInt32, String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Tuple(vec![ColumnData::Uint32(vec![]), ColumnData::String(vec![])]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 0, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert_eq!(elems[0].row_count(), 0);
                assert_eq!(elems[1].row_count(), 0);
            }
            _ => panic!("expected Tuple"),
        }
    }

    #[test]
    fn test_tuple_single_element() {
        // Tuple(Int32) — single-element tuple is legal in ClickHouse.
        let col = Column {
            name: "t".to_string(),
            data_type: "Tuple(Int32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Tuple(vec![ColumnData::Int32(vec![-1, 0, 1])]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Tuple(elems) => {
                assert_eq!(elems.len(), 1);
                match &elems[0] {
                    ColumnData::Int32(v) => assert_eq!(v, &vec![-1, 0, 1]),
                    _ => panic!("expected Int32"),
                }
            }
            _ => panic!("expected Tuple"),
        }
    }

    #[test]
    fn test_tuple_nested_tuple() {
        // Tuple(UInt8, Tuple(Int32, String)) with 2 rows.
        let col = Column {
            name: "t".to_string(),
            data_type: "Tuple(UInt8, Tuple(Int32, String))".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Tuple(vec![
                ColumnData::Uint8(vec![1, 2]),
                ColumnData::Tuple(vec![
                    ColumnData::Int32(vec![100, 200]),
                    ColumnData::String(vec!["x".to_string(), "y".to_string()]),
                ]),
            ]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                match &elems[0] {
                    ColumnData::Uint8(v) => assert_eq!(v, &vec![1, 2]),
                    _ => panic!("expected Uint8"),
                }
                match &elems[1] {
                    ColumnData::Tuple(inner) => {
                        assert_eq!(inner.len(), 2);
                        match &inner[0] {
                            ColumnData::Int32(v) => assert_eq!(v, &vec![100, 200]),
                            _ => panic!("expected Int32"),
                        }
                        match &inner[1] {
                            ColumnData::String(v) => assert_eq!(v, &vec!["x", "y"]),
                            _ => panic!("expected String"),
                        }
                    }
                    _ => panic!("expected nested Tuple"),
                }
            }
            _ => panic!("expected outer Tuple"),
        }
    }

    #[test]
    fn test_tuple_with_array_inner() {
        // Tuple(Array(UInt32), String) — composite-of-composite.
        // 2 rows: ([1,2,3], "hi"), ([4], "bye")
        let col = Column {
            name: "t".to_string(),
            data_type: "Tuple(Array(UInt32), String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Tuple(vec![
                ColumnData::Array {
                    inner: Box::new(ColumnData::Uint32(vec![1, 2, 3, 4])),
                    offsets: vec![3, 4],
                },
                ColumnData::String(vec!["hi".to_string(), "bye".to_string()]),
            ]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                match &elems[0] {
                    ColumnData::Array { inner, offsets } => {
                        assert_eq!(offsets, &vec![3u64, 4]);
                        match inner.as_ref() {
                            ColumnData::Uint32(v) => assert_eq!(v, &vec![1, 2, 3, 4]),
                            _ => panic!("expected Uint32 innermost"),
                        }
                    }
                    _ => panic!("expected Array element"),
                }
                match &elems[1] {
                    ColumnData::String(v) => assert_eq!(v, &vec!["hi", "bye"]),
                    _ => panic!("expected String element"),
                }
            }
            _ => panic!("expected Tuple"),
        }
    }

    #[test]
    fn test_tuple_wire_layout() {
        // Tuple(UInt8, UInt8) with 3 rows: (1,4), (2,5), (3,6).
        // Wire: per-element streams concatenated — first all UInt8 values for
        // element 0, then all UInt8 values for element 1. No length prefix.
        let col = Column {
            name: "t".to_string(),
            data_type: "Tuple(UInt8, UInt8)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Tuple(vec![
                ColumnData::Uint8(vec![1, 2, 3]),
                ColumnData::Uint8(vec![4, 5, 6]),
            ]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        // Last 6 bytes should be: [1,2,3] then [4,5,6].
        let tail = &buf[buf.len() - 6..];
        assert_eq!(tail, &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    }

    #[test]
    fn test_tuple_row_count_is_inner_row_count() {
        // Tuple's row_count must come from inner element columns, not from
        // the count of element types. (Regression: previously v.len() == 2.)
        let c = ColumnData::Tuple(vec![
            ColumnData::Uint32(vec![10, 20, 30, 40, 50]),
            ColumnData::String(vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
                "e".to_string(),
            ]),
        ]);
        assert_eq!(c.row_count(), 5);
    }

    #[test]
    fn test_tuple_row_count_empty() {
        // Empty tuple has 0 rows.
        let c = ColumnData::Tuple(vec![]);
        assert_eq!(c.row_count(), 0);
    }

    #[test]
    fn test_validate_rejects_mismatched_tuple_row_counts() {
        // First element has 3 rows, second has 2 — invariant broken.
        let c = ColumnData::Tuple(vec![
            ColumnData::Uint32(vec![1, 2, 3]),
            ColumnData::String(vec!["a".to_string(), "b".to_string()]),
        ]);
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_recurses_into_tuple_inner() {
        // Outer Tuple row counts match (both 2 outer rows), but the inner
        // Array offsets are non-monotonic — must be caught by recursion.
        let c = ColumnData::Tuple(vec![
            ColumnData::Array {
                inner: Box::new(ColumnData::Uint32(vec![1, 2, 3])),
                offsets: vec![3, 1], // non-monotonic
            },
            ColumnData::String(vec!["a".to_string(), "b".to_string()]),
        ]);
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_split_with_composite_flat() {
        let got = split_with_composite("Int8, Int64, Float32").unwrap();
        assert_eq!(got, vec!["Int8", "Int64", "Float32"]);
    }

    #[test]
    fn test_split_with_composite_nested() {
        let input =
            "Int8, Int64, Tuple(Float32, String), String, Tuple(String, Tuple(Int8, String))";
        let expected = vec![
            "Int8",
            "Int64",
            "Tuple(Float32, String)",
            "String",
            "Tuple(String, Tuple(Int8, String))",
        ];
        assert_eq!(split_with_composite(input).unwrap(), expected);
    }

    #[test]
    fn test_split_with_composite_single() {
        // No commas — single piece returned as-is.
        let got = split_with_composite("Int32").unwrap();
        assert_eq!(got, vec!["Int32"]);
    }

    #[test]
    fn test_split_with_composite_array_inside() {
        // Comma inside Array(...) must NOT split the outer.
        // (Today Array takes a single inner type, but the splitter must still
        // respect depth so Tuple(Array(UInt32), String) and similar work.)
        let got = split_with_composite("Array(UInt32), String").unwrap();
        assert_eq!(got, vec!["Array(UInt32)", "String"]);
    }

    #[test]
    fn test_split_with_composite_unbalanced_parens() {
        // Missing close paren — depth ends > 0, must error.
        assert!(split_with_composite("Tuple(Int8, Int32").is_err());
    }

    #[test]
    fn test_parse_tuple_inner_types_basic() {
        let got = parse_tuple_inner_types("Tuple(Int8, String, UInt32)").unwrap();
        assert_eq!(got, vec!["Int8", "String", "UInt32"]);
    }

    #[test]
    fn test_parse_tuple_inner_types_nested() {
        let got = parse_tuple_inner_types("Tuple(Tuple(Int8, Int32), String)").unwrap();
        assert_eq!(got, vec!["Tuple(Int8, Int32)", "String"]);
    }

    #[test]
    fn test_parse_tuple_inner_types_invalid() {
        assert!(parse_tuple_inner_types("Tuple").is_err());
        assert!(parse_tuple_inner_types("Tuple()").is_err());
    }

    // -- Map(K, V) --

    #[test]
    fn test_map_string_uint32_roundtrip() {
        // Map(String, UInt32) with 2 rows: {'a':1, 'b':2}, {'c':3}
        let col = Column {
            name: "m".to_string(),
            data_type: "Map(String, UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Map {
                keys: Box::new(ColumnData::String(vec![
                    "a".to_string(),
                    "b".to_string(),
                    "c".to_string(),
                ])),
                values: Box::new(ColumnData::Uint32(vec![1, 2, 3])),
                offsets: vec![2, 3],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Map {
                keys,
                values,
                offsets,
            } => {
                assert_eq!(offsets, vec![2, 3]);
                match *keys {
                    ColumnData::String(v) => assert_eq!(v, vec!["a", "b", "c"]),
                    _ => panic!("expected String keys"),
                }
                match *values {
                    ColumnData::Uint32(v) => assert_eq!(v, vec![1u32, 2, 3]),
                    _ => panic!("expected Uint32 values"),
                }
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn test_map_zero_rows() {
        let col = Column {
            name: "m".to_string(),
            data_type: "Map(String, UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Map {
                keys: Box::new(ColumnData::String(vec![])),
                values: Box::new(ColumnData::Uint32(vec![])),
                offsets: vec![],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 0, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Map {
                keys,
                values,
                offsets,
            } => {
                assert!(offsets.is_empty());
                assert_eq!(keys.row_count(), 0);
                assert_eq!(values.row_count(), 0);
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn test_map_empty_row() {
        // Single row with an empty map.
        let col = Column {
            name: "m".to_string(),
            data_type: "Map(String, UInt32)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Map {
                keys: Box::new(ColumnData::String(vec![])),
                values: Box::new(ColumnData::Uint32(vec![])),
                offsets: vec![0],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Map { offsets, .. } => assert_eq!(offsets, vec![0]),
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn test_map_complex_value_array() {
        // Map(String, Array(UInt32)) with 1 row: {'a':[1,2], 'b':[]}
        let col = Column {
            name: "m".to_string(),
            data_type: "Map(String, Array(UInt32))".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Map {
                keys: Box::new(ColumnData::String(vec!["a".to_string(), "b".to_string()])),
                values: Box::new(ColumnData::Array {
                    inner: Box::new(ColumnData::Uint32(vec![1, 2])),
                    offsets: vec![2, 2],
                }),
                offsets: vec![2],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Map {
                keys,
                values,
                offsets,
            } => {
                assert_eq!(offsets, vec![2]);
                match *keys {
                    ColumnData::String(v) => assert_eq!(v, vec!["a", "b"]),
                    _ => panic!("expected String keys"),
                }
                match *values {
                    ColumnData::Array {
                        inner,
                        offsets: vo,
                    } => {
                        assert_eq!(vo, vec![2, 2]);
                        match *inner {
                            ColumnData::Uint32(v) => assert_eq!(v, vec![1u32, 2]),
                            _ => panic!("expected Uint32 innermost"),
                        }
                    }
                    _ => panic!("expected Array values"),
                }
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn test_map_wire_layout() {
        // Map(UInt8, UInt8) with 2 rows: {1:10, 2:20}, {3:30}.
        // Wire: 2 × UInt64 LE offsets, then 3 keys, then 3 values.
        let col = Column {
            name: "m".to_string(),
            data_type: "Map(UInt8, UInt8)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Map {
                keys: Box::new(ColumnData::Uint8(vec![1, 2, 3])),
                values: Box::new(ColumnData::Uint8(vec![10, 20, 30])),
                offsets: vec![2, 3],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        // Tail: [2,0,0,0,0,0,0,0] [3,0,0,0,0,0,0,0] [1,2,3] [10,20,30]
        let expected_tail = vec![
            0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // offsets[0] = 2
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // offsets[1] = 3
            0x01, 0x02, 0x03, // keys
            0x0A, 0x14, 0x1E, // values: 10, 20, 30
        ];
        let tail = &buf[buf.len() - expected_tail.len()..];
        assert_eq!(tail, expected_tail.as_slice());
    }

    #[test]
    fn test_map_row_count_is_offsets_len() {
        let c = ColumnData::Map {
            keys: Box::new(ColumnData::String(vec!["a".to_string(), "b".to_string()])),
            values: Box::new(ColumnData::Uint32(vec![1, 2])),
            offsets: vec![1, 2],
        };
        assert_eq!(c.row_count(), 2);
    }

    #[test]
    fn test_validate_rejects_map_keys_values_mismatch() {
        // 2 keys but 3 values — invariant broken.
        let c = ColumnData::Map {
            keys: Box::new(ColumnData::String(vec!["a".to_string(), "b".to_string()])),
            values: Box::new(ColumnData::Uint32(vec![1, 2, 3])),
            offsets: vec![2],
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_rejects_map_offset_keys_mismatch() {
        // offsets.last() = 3 but only 2 keys.
        let c = ColumnData::Map {
            keys: Box::new(ColumnData::String(vec!["a".to_string(), "b".to_string()])),
            values: Box::new(ColumnData::Uint32(vec![1, 2])),
            offsets: vec![3],
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_rejects_map_non_monotonic_offsets() {
        let c = ColumnData::Map {
            keys: Box::new(ColumnData::String(vec!["a".to_string(), "b".to_string(), "c".to_string()])),
            values: Box::new(ColumnData::Uint32(vec![1, 2, 3])),
            offsets: vec![2, 1, 3], // 1 < 2 — invalid
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_recurses_into_map_inner() {
        // Outer Map invariants pass (3 keys, 3 values, monotonic offsets), but
        // the inner Array used as the values column has bad offsets.
        let c = ColumnData::Map {
            keys: Box::new(ColumnData::String(vec!["a".to_string()])),
            values: Box::new(ColumnData::Array {
                inner: Box::new(ColumnData::Uint32(vec![1, 2, 3])),
                offsets: vec![3, 1], // non-monotonic
            }),
            offsets: vec![1],
        };
        // Top-level Map invariant: keys.row_count()=1, values.row_count()=2 (Array
        // row count = offsets.len() = 2). That mismatch is caught first; force
        // the outer invariant to pass by aligning row counts.
        // We need a setup where outer is valid but inner is not.
        let _ = c;

        // Construct: 1 row of map, with values column = Array{ offsets=[3,1] }.
        // For the Map invariant to pass, values.row_count() must equal
        // offsets.last()=1. Array row count is offsets.len(); we need a single
        // outer row, so values.offsets must have len=1. But a single offset
        // can't be non-monotonic. So instead: 2 rows of map, map.offsets=[1,2],
        // values.offsets=[2,1] (len=2 ✓, but 1<2 invalid).
        let c2 = ColumnData::Map {
            keys: Box::new(ColumnData::String(vec!["a".to_string(), "b".to_string()])),
            values: Box::new(ColumnData::Array {
                inner: Box::new(ColumnData::Uint32(vec![1, 2])),
                offsets: vec![2, 1], // non-monotonic
            }),
            offsets: vec![1, 2],
        };
        let mut buf = Vec::new();
        let err = c2.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_parse_map_inner_types_basic() {
        let (k, v) = parse_map_inner_types("Map(String, UInt32)").unwrap();
        assert_eq!(k, "String");
        assert_eq!(v, "UInt32");
    }

    #[test]
    fn test_parse_map_inner_types_nested() {
        let (k, v) = parse_map_inner_types("Map(String, Array(UInt32))").unwrap();
        assert_eq!(k, "String");
        assert_eq!(v, "Array(UInt32)");

        let (k2, v2) = parse_map_inner_types("Map(String, Tuple(Int32, String))").unwrap();
        assert_eq!(k2, "String");
        assert_eq!(v2, "Tuple(Int32, String)");
    }

    #[test]
    fn test_parse_map_inner_types_invalid() {
        assert!(parse_map_inner_types("Map").is_err());
        assert!(parse_map_inner_types("Map()").is_err());
        // Wrong arity — only one type.
        assert!(parse_map_inner_types("Map(String)").is_err());
        // Wrong arity — three types.
        assert!(parse_map_inner_types("Map(String, Int32, UInt32)").is_err());
    }

    // -- Nested(...) --

    #[test]
    fn test_nested_uint8_string_roundtrip() {
        // Nested(a UInt8, b String) with 2 rows:
        //   row 0: a=[10,20], b=['x','y']
        //   row 1: a=[30],    b=['z']
        let col = Column {
            name: "n".to_string(),
            data_type: "Nested(a UInt8, b String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nested {
                fields: vec![
                    ("a".to_string(), ColumnData::Uint8(vec![10, 20, 30])),
                    (
                        "b".to_string(),
                        ColumnData::String(vec![
                            "x".to_string(),
                            "y".to_string(),
                            "z".to_string(),
                        ]),
                    ),
                ],
                offsets: vec![2, 3],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nested { fields, offsets } => {
                assert_eq!(offsets, vec![2, 3]);
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].0, "a");
                match &fields[0].1 {
                    ColumnData::Uint8(v) => assert_eq!(v, &vec![10, 20, 30]),
                    _ => panic!("expected Uint8 field a"),
                }
                assert_eq!(fields[1].0, "b");
                match &fields[1].1 {
                    ColumnData::String(v) => assert_eq!(v, &vec!["x", "y", "z"]),
                    _ => panic!("expected String field b"),
                }
            }
            _ => panic!("expected Nested"),
        }
    }

    #[test]
    fn test_nested_zero_rows() {
        let col = Column {
            name: "n".to_string(),
            data_type: "Nested(a UInt8, b String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nested {
                fields: vec![
                    ("a".to_string(), ColumnData::Uint8(vec![])),
                    ("b".to_string(), ColumnData::String(vec![])),
                ],
                offsets: vec![],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 0, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nested { fields, offsets } => {
                assert!(offsets.is_empty());
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].1.row_count(), 0);
                assert_eq!(fields[1].1.row_count(), 0);
            }
            _ => panic!("expected Nested"),
        }
    }

    #[test]
    fn test_nested_three_fields() {
        // Nested(x Int32, y UInt8, z String), 1 row, 2 elements per row.
        let col = Column {
            name: "n".to_string(),
            data_type: "Nested(x Int32, y UInt8, z String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nested {
                fields: vec![
                    ("x".to_string(), ColumnData::Int32(vec![-1, -2])),
                    ("y".to_string(), ColumnData::Uint8(vec![1, 2])),
                    (
                        "z".to_string(),
                        ColumnData::String(vec!["one".to_string(), "two".to_string()]),
                    ),
                ],
                offsets: vec![2],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nested { fields, offsets } => {
                assert_eq!(offsets, vec![2]);
                assert_eq!(fields.len(), 3);
                let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
                assert_eq!(names, vec!["x", "y", "z"]);
            }
            _ => panic!("expected Nested"),
        }
    }

    #[test]
    fn test_nested_with_array_field() {
        // Nested(a UInt8, b Array(UInt32)) — field type itself is composite.
        // 1 row with 2 elements: a=[1, 2], b=[[10, 20], [30]]
        let col = Column {
            name: "n".to_string(),
            data_type: "Nested(a UInt8, b Array(UInt32))".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nested {
                fields: vec![
                    ("a".to_string(), ColumnData::Uint8(vec![1, 2])),
                    (
                        "b".to_string(),
                        ColumnData::Array {
                            inner: Box::new(ColumnData::Uint32(vec![10, 20, 30])),
                            offsets: vec![2, 3],
                        },
                    ),
                ],
                offsets: vec![2],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nested { fields, offsets } => {
                assert_eq!(offsets, vec![2]);
                assert_eq!(fields.len(), 2);
                match &fields[1].1 {
                    ColumnData::Array {
                        inner,
                        offsets: bo,
                    } => {
                        assert_eq!(bo, &vec![2u64, 3]);
                        match inner.as_ref() {
                            ColumnData::Uint32(v) => assert_eq!(v, &vec![10u32, 20, 30]),
                            _ => panic!("expected Uint32 innermost"),
                        }
                    }
                    _ => panic!("expected Array field b"),
                }
            }
            _ => panic!("expected Nested"),
        }
    }

    #[test]
    fn test_nested_wire_layout_matches_array_tuple() {
        // Regression / documentation: encoded Nested(...) bytes after the type
        // string must match what Array(Tuple(...)) would emit for the same data.
        let nested = Column {
            name: "n".to_string(),
            data_type: "Nested(a UInt8, b UInt8)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nested {
                fields: vec![
                    ("a".to_string(), ColumnData::Uint8(vec![10, 20, 30])),
                    ("b".to_string(), ColumnData::Uint8(vec![40, 50, 60])),
                ],
                offsets: vec![2, 3],
            },
        };
        let array_tuple = Column {
            name: "n".to_string(),
            // Same bytes regardless of which type string is written, after the
            // type string ends. We don't compare the type-string bytes.
            data_type: "Array(Tuple(UInt8, UInt8))".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Array {
                inner: Box::new(ColumnData::Tuple(vec![
                    ColumnData::Uint8(vec![10, 20, 30]),
                    ColumnData::Uint8(vec![40, 50, 60]),
                ])),
                offsets: vec![2, 3],
            },
        };

        let mut buf_nested = Vec::new();
        nested.encode(&mut buf_nested, PROTOCOL).unwrap();
        let mut buf_at = Vec::new();
        array_tuple.encode(&mut buf_at, PROTOCOL).unwrap();

        // The trailing 22 bytes (offsets × 16, per-element streams × 6) must be
        // identical:
        let tail_len = 22;
        assert_eq!(
            &buf_nested[buf_nested.len() - tail_len..],
            &buf_at[buf_at.len() - tail_len..],
        );
    }

    #[test]
    fn test_nested_row_count_is_offsets_len() {
        let c = ColumnData::Nested {
            fields: vec![
                ("a".to_string(), ColumnData::Uint8(vec![1, 2, 3])),
                ("b".to_string(), ColumnData::Uint8(vec![4, 5, 6])),
            ],
            offsets: vec![1, 2, 3],
        };
        assert_eq!(c.row_count(), 3);
    }

    #[test]
    fn test_validate_rejects_nested_field_row_count_mismatch() {
        // Field 'a' has 3 values, field 'b' has 2 — invariant broken.
        let c = ColumnData::Nested {
            fields: vec![
                ("a".to_string(), ColumnData::Uint8(vec![1, 2, 3])),
                ("b".to_string(), ColumnData::Uint8(vec![4, 5])),
            ],
            offsets: vec![3],
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_rejects_nested_offset_field_mismatch() {
        // offsets.last() = 3 but each field has only 2 values.
        let c = ColumnData::Nested {
            fields: vec![
                ("a".to_string(), ColumnData::Uint8(vec![1, 2])),
                ("b".to_string(), ColumnData::Uint8(vec![3, 4])),
            ],
            offsets: vec![3],
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_rejects_nested_non_monotonic_offsets() {
        let c = ColumnData::Nested {
            fields: vec![
                ("a".to_string(), ColumnData::Uint8(vec![1, 2, 3])),
                ("b".to_string(), ColumnData::Uint8(vec![4, 5, 6])),
            ],
            offsets: vec![2, 1, 3], // 1 < 2 — invalid
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_recurses_into_nested_field() {
        // Nested invariants pass at the outer level (1 row, offsets=[2], each
        // field row_count=2), but the Array field has non-monotonic offsets.
        let c = ColumnData::Nested {
            fields: vec![
                ("a".to_string(), ColumnData::Uint8(vec![1, 2])),
                (
                    "b".to_string(),
                    ColumnData::Array {
                        inner: Box::new(ColumnData::Uint32(vec![10, 20])),
                        offsets: vec![2, 1], // non-monotonic
                    },
                ),
            ],
            offsets: vec![2],
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_parse_nested_inner_types_basic() {
        let got = parse_nested_inner_types("Nested(a UInt8, b String)").unwrap();
        assert_eq!(
            got,
            vec![
                ("a".to_string(), "UInt8".to_string()),
                ("b".to_string(), "String".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_nested_inner_types_complex_field() {
        // Field type contains internal commas — depth-aware splitter handles it.
        let got = parse_nested_inner_types(
            "Nested(a Tuple(x UInt8, y String), b Array(UInt32))",
        )
        .unwrap();
        assert_eq!(
            got,
            vec![
                ("a".to_string(), "Tuple(x UInt8, y String)".to_string()),
                ("b".to_string(), "Array(UInt32)".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_nested_inner_types_extra_whitespace() {
        // Multiple spaces between name and type, leading/trailing whitespace.
        let got = parse_nested_inner_types("Nested(  a   UInt8 ,  b   String  )").unwrap();
        assert_eq!(
            got,
            vec![
                ("a".to_string(), "UInt8".to_string()),
                ("b".to_string(), "String".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_nested_inner_types_invalid() {
        // Missing type for a field.
        assert!(parse_nested_inner_types("Nested(a)").is_err());
        // Empty inner.
        assert!(parse_nested_inner_types("Nested()").is_err());
        // No parens at all.
        assert!(parse_nested_inner_types("Nested").is_err());
    }

    // -- Phase 7: Int16 / Float32 / Float64 / Bool --

    #[test]
    fn test_int16_roundtrip() {
        let col = Column {
            name: "x".to_string(),
            data_type: "Int16".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Int16(vec![-32768, -1, 0, 1, 32767]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 5, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Int16(v) => assert_eq!(v, vec![-32768, -1, 0, 1, 32767]),
            _ => panic!("expected Int16"),
        }
    }

    #[test]
    fn test_int16_wire_layout() {
        // Probe-confirmed: -1 → ff ff (LE i16).
        let col = Column {
            name: "x".to_string(),
            data_type: "Int16".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Int16(vec![-1]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 2..];
        assert_eq!(tail, &[0xFF, 0xFF]);
    }

    #[test]
    fn test_float32_roundtrip() {
        let col = Column {
            name: "x".to_string(),
            data_type: "Float32".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Float32(vec![1.5, -1.5, 0.0, f32::INFINITY]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 4, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Float32(v) => {
                assert_eq!(v[0], 1.5);
                assert_eq!(v[1], -1.5);
                assert_eq!(v[2], 0.0);
                assert!(v[3].is_infinite() && v[3] > 0.0);
            }
            _ => panic!("expected Float32"),
        }
    }

    #[test]
    fn test_float32_wire_layout() {
        // Probe-confirmed: 1.5 → 00 00 c0 3f (IEEE 754 LE).
        let col = Column {
            name: "x".to_string(),
            data_type: "Float32".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Float32(vec![1.5]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 4..];
        assert_eq!(tail, &[0x00, 0x00, 0xC0, 0x3F]);
    }

    #[test]
    fn test_float32_nan_roundtrip() {
        let col = Column {
            name: "x".to_string(),
            data_type: "Float32".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Float32(vec![f32::NAN]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Float32(v) => assert!(v[0].is_nan()),
            _ => panic!("expected Float32"),
        }
    }

    #[test]
    fn test_float64_roundtrip() {
        let col = Column {
            name: "x".to_string(),
            data_type: "Float64".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Float64(vec![1.5, -2.5, 1e100, 0.0]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 4, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Float64(v) => {
                assert_eq!(v, vec![1.5, -2.5, 1e100, 0.0]);
            }
            _ => panic!("expected Float64"),
        }
    }

    #[test]
    fn test_float64_wire_layout() {
        // Probe-confirmed: 1.5 → 00 00 00 00 00 00 f8 3f (IEEE 754 LE).
        let col = Column {
            name: "x".to_string(),
            data_type: "Float64".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Float64(vec![1.5]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 8..];
        assert_eq!(tail, &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF8, 0x3F]);
    }

    #[test]
    fn test_bool_roundtrip() {
        let col = Column {
            name: "x".to_string(),
            data_type: "Bool".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Bool(vec![true, false, true]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Bool(v) => assert_eq!(v, vec![true, false, true]),
            _ => panic!("expected Bool"),
        }
    }

    #[test]
    fn test_bool_wire_layout() {
        // Probe-confirmed: [true, false, true] → 01 00 01 (1 byte each).
        let col = Column {
            name: "x".to_string(),
            data_type: "Bool".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Bool(vec![true, false, true]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 3..];
        assert_eq!(tail, &[0x01, 0x00, 0x01]);
    }

    // -- Phase 7: Date / Date32 --

    #[test]
    fn test_date_roundtrip() {
        let col = Column {
            name: "d".to_string(),
            data_type: "Date".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Date(vec![0, 1, 19737, 65535]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 4, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Date(v) => assert_eq!(v, vec![0, 1, 19737, 65535]),
            _ => panic!("expected Date"),
        }
    }

    #[test]
    fn test_date_wire_layout() {
        // Probe-confirmed: 1970-01-02 → 1 day → 01 00 (UInt16 LE).
        let col = Column {
            name: "d".to_string(),
            data_type: "Date".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Date(vec![1]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 2..];
        assert_eq!(tail, &[0x01, 0x00]);
    }

    #[test]
    fn test_date32_roundtrip() {
        let col = Column {
            name: "d".to_string(),
            data_type: "Date32".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Date32(vec![-25567, -1, 0, 19723]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 4, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Date32(v) => assert_eq!(v, vec![-25567, -1, 0, 19723]),
            _ => panic!("expected Date32"),
        }
    }

    #[test]
    fn test_date32_wire_layout() {
        // Probe-confirmed: 1900-01-01 → -25567 days → 21 9c ff ff (Int32 LE).
        let col = Column {
            name: "d".to_string(),
            data_type: "Date32".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Date32(vec![-25567]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 4..];
        assert_eq!(tail, &[0x21, 0x9C, 0xFF, 0xFF]);
    }

    // -- Phase 7: DateTime64 --

    #[test]
    fn test_datetime64_scale_3_roundtrip() {
        // 2024-01-15 12:30:45.123 UTC → 1705321845123 ms.
        let col = Column {
            name: "ts".to_string(),
            data_type: "DateTime64(3, 'UTC')".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::DateTime64 {
                scale: 3,
                values: vec![1705321845123],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::DateTime64 { scale, values } => {
                assert_eq!(scale, 3);
                assert_eq!(values, vec![1705321845123]);
            }
            _ => panic!("expected DateTime64"),
        }
    }

    #[test]
    fn test_datetime64_scale_0_no_tz() {
        // 2024-01-15 12:30:45 UTC → 1705321845 seconds.
        let col = Column {
            name: "ts".to_string(),
            data_type: "DateTime64(0)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::DateTime64 {
                scale: 0,
                values: vec![1705321845],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::DateTime64 { scale, values } => {
                assert_eq!(scale, 0);
                assert_eq!(values, vec![1705321845]);
            }
            _ => panic!("expected DateTime64"),
        }
    }

    #[test]
    fn test_parse_datetime64_scale() {
        assert_eq!(parse_datetime64_scale("DateTime64(0)").unwrap(), 0);
        assert_eq!(parse_datetime64_scale("DateTime64(3)").unwrap(), 3);
        assert_eq!(parse_datetime64_scale("DateTime64(9)").unwrap(), 9);
        assert_eq!(
            parse_datetime64_scale("DateTime64(6, 'America/Los_Angeles')").unwrap(),
            6
        );
        assert_eq!(parse_datetime64_scale("DateTime64( 3 , 'UTC')").unwrap(), 3);
    }

    #[test]
    fn test_parse_datetime64_scale_invalid() {
        assert!(parse_datetime64_scale("DateTime64").is_err());
        assert!(parse_datetime64_scale("DateTime64()").is_err());
        assert!(parse_datetime64_scale("DateTime64(10)").is_err()); // out of range
        assert!(parse_datetime64_scale("DateTime64(abc)").is_err());
    }

    // -- Phase 7: UUID --

    #[test]
    fn test_uuid_roundtrip() {
        let u1 = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let u2 = Uuid::parse_str("00000000-0000-0000-0000-000000000000").unwrap();
        let u3 = Uuid::parse_str("ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap();
        let col = Column {
            name: "u".to_string(),
            data_type: "UUID".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uuid(vec![u1, u2, u3]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Uuid(v) => assert_eq!(v, vec![u1, u2, u3]),
            _ => panic!("expected UUID"),
        }
    }

    #[test]
    fn test_uuid_wire_layout_byte_swap() {
        // Probe-confirmed: 550e8400-e29b-41d4-a716-446655440000 →
        //   d4 41 9b e2 00 84 0e 55 00 00 44 55 66 44 16 a7
        // (each 8-byte half byte-reversed from canonical big-endian)
        let u = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let col = Column {
            name: "u".to_string(),
            data_type: "UUID".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uuid(vec![u]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 16..];
        assert_eq!(
            tail,
            &[
                0xD4, 0x41, 0x9B, 0xE2, 0x00, 0x84, 0x0E, 0x55, // high half byte-reversed
                0x00, 0x00, 0x44, 0x55, 0x66, 0x44, 0x16, 0xA7, // low half byte-reversed
            ]
        );
    }

    // -- Phase 7: IPv4 / IPv6 --

    #[test]
    fn test_ipv4_roundtrip() {
        let col = Column {
            name: "ip".to_string(),
            data_type: "IPv4".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Ipv4(vec![0xC0A8010A, 0x7F000001, 0]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Ipv4(v) => assert_eq!(v, vec![0xC0A8010A, 0x7F000001, 0]),
            _ => panic!("expected IPv4"),
        }
    }

    #[test]
    fn test_ipv4_wire_layout() {
        // Probe-confirmed: 192.168.1.10 → u32 = 0xC0A8010A → wire bytes
        // 0a 01 a8 c0 (LE).
        let col = Column {
            name: "ip".to_string(),
            data_type: "IPv4".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Ipv4(vec![0xC0A8010A]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 4..];
        assert_eq!(tail, &[0x0A, 0x01, 0xA8, 0xC0]);
    }

    #[test]
    fn test_ipv6_roundtrip() {
        // 2001:db8::1
        let mut addr = [0u8; 16];
        addr[0] = 0x20;
        addr[1] = 0x01;
        addr[2] = 0x0D;
        addr[3] = 0xB8;
        addr[15] = 0x01;
        let col = Column {
            name: "ip".to_string(),
            data_type: "IPv6".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Ipv6(vec![addr]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Ipv6(v) => assert_eq!(v, vec![addr]),
            _ => panic!("expected IPv6"),
        }
    }

    #[test]
    fn test_ipv6_wire_layout() {
        // Probe-confirmed: 2001:db8::1 → 16 bytes verbatim in network order.
        let mut addr = [0u8; 16];
        addr[0] = 0x20;
        addr[1] = 0x01;
        addr[2] = 0x0D;
        addr[3] = 0xB8;
        addr[15] = 0x01;
        let col = Column {
            name: "ip".to_string(),
            data_type: "IPv6".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Ipv6(vec![addr]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 16..];
        assert_eq!(
            tail,
            &[
                0x20, 0x01, 0x0D, 0xB8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x01,
            ]
        );
    }

    // -- Phase 7: Enum16 --

    #[test]
    fn test_enum16_roundtrip() {
        let col = Column {
            name: "e".to_string(),
            data_type: "Enum16('a' = 1, 'b' = 30000)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Enum16(vec![1, 30000, -1]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Enum16(v) => assert_eq!(v, vec![1, 30000, -1]),
            _ => panic!("expected Enum16"),
        }
    }

    #[test]
    fn test_enum16_wire_layout() {
        // Probe-confirmed: 30000 → 30 75 (Int16 LE).
        let col = Column {
            name: "e".to_string(),
            data_type: "Enum16('a' = 1, 'b' = 30000)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Enum16(vec![30000]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 2..];
        assert_eq!(tail, &[0x30, 0x75]);
    }

    // -- Phase 7: Decimal --

    #[test]
    fn test_decimal32_roundtrip() {
        // 123.4567 with scale 4 → 1234567.
        let col = Column {
            name: "d".to_string(),
            data_type: "Decimal(9, 4)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Decimal32 {
                scale: 4,
                values: vec![1234567, -1, 0],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Decimal32 { scale, values } => {
                assert_eq!(scale, 4);
                assert_eq!(values, vec![1234567, -1, 0]);
            }
            _ => panic!("expected Decimal32"),
        }
    }

    #[test]
    fn test_decimal32_wire_layout() {
        // Probe-confirmed: 123.4567 with scale 4 → 1234567 → 87 d6 12 00.
        let col = Column {
            name: "d".to_string(),
            data_type: "Decimal(9, 4)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Decimal32 {
                scale: 4,
                values: vec![1234567],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 4..];
        assert_eq!(tail, &[0x87, 0xD6, 0x12, 0x00]);
    }

    #[test]
    fn test_decimal64_roundtrip_negative() {
        // -1.5 with scale 1 → -15.
        let col = Column {
            name: "d".to_string(),
            data_type: "Decimal(18, 1)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Decimal64 {
                scale: 1,
                values: vec![-15],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Decimal64 { scale, values } => {
                assert_eq!(scale, 1);
                assert_eq!(values, vec![-15]);
            }
            _ => panic!("expected Decimal64"),
        }
    }

    #[test]
    fn test_decimal128_roundtrip() {
        let col = Column {
            name: "d".to_string(),
            data_type: "Decimal(38, 4)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Decimal128 {
                scale: 4,
                values: vec![1234567],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Decimal128 { scale, values } => {
                assert_eq!(scale, 4);
                assert_eq!(values, vec![1234567]);
            }
            _ => panic!("expected Decimal128"),
        }
    }

    #[test]
    fn test_decimal256_roundtrip() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0x87;
        bytes[1] = 0xD6;
        bytes[2] = 0x12;
        let col = Column {
            name: "d".to_string(),
            data_type: "Decimal(76, 4)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Decimal256 {
                scale: 4,
                values: vec![bytes],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 1, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Decimal256 { scale, values } => {
                assert_eq!(scale, 4);
                assert_eq!(values, vec![bytes]);
            }
            _ => panic!("expected Decimal256"),
        }
    }

    #[test]
    fn test_parse_decimal_p_s() {
        assert_eq!(parse_decimal_p_s("Decimal(9, 4)").unwrap(), (9, 4));
        assert_eq!(parse_decimal_p_s("Decimal(18, 0)").unwrap(), (18, 0));
        assert_eq!(parse_decimal_p_s("Decimal(76, 38)").unwrap(), (76, 38));
        assert_eq!(parse_decimal_p_s("Decimal( 38 , 4 )").unwrap(), (38, 4));
    }

    #[test]
    fn test_parse_decimal_p_s_invalid() {
        assert!(parse_decimal_p_s("Decimal").is_err());
        assert!(parse_decimal_p_s("Decimal()").is_err());
        assert!(parse_decimal_p_s("Decimal(9)").is_err());
        assert!(parse_decimal_p_s("Decimal(9, 4, 1)").is_err());
        assert!(parse_decimal_p_s("Decimal(abc, 4)").is_err());
    }

    #[test]
    fn test_decimal_byte_width() {
        assert_eq!(decimal_byte_width(1).unwrap(), 4);
        assert_eq!(decimal_byte_width(9).unwrap(), 4);
        assert_eq!(decimal_byte_width(10).unwrap(), 8);
        assert_eq!(decimal_byte_width(18).unwrap(), 8);
        assert_eq!(decimal_byte_width(19).unwrap(), 16);
        assert_eq!(decimal_byte_width(38).unwrap(), 16);
        assert_eq!(decimal_byte_width(39).unwrap(), 32);
        assert_eq!(decimal_byte_width(76).unwrap(), 32);
        assert!(decimal_byte_width(0).is_err());
        assert!(decimal_byte_width(77).is_err());
    }

    // -- Phase 7: Int128 / UInt128 / Int256 / UInt256 --

    #[test]
    fn test_int128_roundtrip() {
        let col = Column {
            name: "x".to_string(),
            data_type: "Int128".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Int128(vec![i128::MAX, i128::MIN, 0, -1]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 4, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Int128(v) => assert_eq!(v, vec![i128::MAX, i128::MIN, 0, -1]),
            _ => panic!("expected Int128"),
        }
    }

    #[test]
    fn test_int128_wire_layout() {
        // Probe-confirmed: i128::MAX → ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff 7f
        let col = Column {
            name: "x".to_string(),
            data_type: "Int128".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Int128(vec![i128::MAX]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let tail = &buf[buf.len() - 16..];
        let mut expected = [0xFFu8; 16];
        expected[15] = 0x7F;
        assert_eq!(tail, &expected);
    }

    #[test]
    fn test_uint128_roundtrip() {
        let col = Column {
            name: "x".to_string(),
            data_type: "UInt128".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uint128(vec![u128::MAX, 0, 1]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Uint128(v) => assert_eq!(v, vec![u128::MAX, 0, 1]),
            _ => panic!("expected UInt128"),
        }
    }

    #[test]
    fn test_int256_roundtrip() {
        let mut a = [0u8; 32];
        a[0] = 0x7B; // 123
        let b = [0xFFu8; 32]; // -1 in two's complement
        let col = Column {
            name: "x".to_string(),
            data_type: "Int256".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Int256(vec![a, b]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Int256(v) => assert_eq!(v, vec![a, b]),
            _ => panic!("expected Int256"),
        }
    }

    #[test]
    fn test_uint256_roundtrip() {
        let mut a = [0u8; 32];
        a[0] = 0x7B;
        let b = [0xFFu8; 32];
        let col = Column {
            name: "x".to_string(),
            data_type: "UInt256".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Uint256(vec![a, b]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Uint256(v) => assert_eq!(v, vec![a, b]),
            _ => panic!("expected UInt256"),
        }
    }

    // Round-trip composability: Phase 7 types as inner types of composites.

    #[test]
    fn test_array_of_float64_roundtrip() {
        let col = Column {
            name: "a".to_string(),
            data_type: "Array(Float64)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Array {
                inner: Box::new(ColumnData::Float64(vec![1.5, -2.5, 3.0, 4.5])),
                offsets: vec![2, 4],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 2, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Array { inner, offsets } => {
                assert_eq!(offsets, vec![2, 4]);
                match *inner {
                    ColumnData::Float64(v) => assert_eq!(v, vec![1.5, -2.5, 3.0, 4.5]),
                    _ => panic!("expected Float64 inner"),
                }
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_nullable_uuid_roundtrip() {
        let u = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let zero = Uuid::nil();
        let col = Column {
            name: "u".to_string(),
            data_type: "Nullable(UUID)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Nullable {
                inner: Box::new(ColumnData::Uuid(vec![u, zero, u])),
                nulls: vec![0, 1, 0],
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Nullable { inner, nulls } => {
                assert_eq!(nulls, vec![0, 1, 0]);
                match *inner {
                    ColumnData::Uuid(v) => assert_eq!(v, vec![u, zero, u]),
                    _ => panic!("expected Uuid inner"),
                }
            }
            _ => panic!("expected Nullable"),
        }
    }

    // -- Phase 8: LowCardinality(T) --

    #[test]
    fn test_lowcardinality_string_roundtrip() {
        let col = Column {
            name: "lc".to_string(),
            data_type: "LowCardinality(String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::LowCardinality {
                dict: Box::new(ColumnData::String(vec![
                    "".to_string(),
                    "a".to_string(),
                    "b".to_string(),
                ])),
                keys: vec![1, 2, 1],
                key_width: 1,
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::LowCardinality {
                dict,
                keys,
                key_width,
            } => {
                assert_eq!(key_width, 1);
                assert_eq!(keys, vec![1u64, 2, 1]);
                match *dict {
                    ColumnData::String(v) => assert_eq!(v, vec!["", "a", "b"]),
                    _ => panic!("expected String dict"),
                }
            }
            _ => panic!("expected LowCardinality"),
        }
    }

    #[test]
    fn test_lowcardinality_zero_rows() {
        let col = Column {
            name: "lc".to_string(),
            data_type: "LowCardinality(String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::LowCardinality {
                dict: Box::new(ColumnData::String(vec![])),
                keys: vec![],
                key_width: 1,
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 0, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::LowCardinality { keys, .. } => assert!(keys.is_empty()),
            _ => panic!("expected LowCardinality"),
        }
    }

    #[test]
    fn test_lowcardinality_uint16_keys() {
        let dict_strings: Vec<String> = (0..300).map(|i| format!("v{i}")).collect();
        let keys: Vec<u64> = (0..300).collect();
        let col = Column {
            name: "lc".to_string(),
            data_type: "LowCardinality(String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::LowCardinality {
                dict: Box::new(ColumnData::String(dict_strings.clone())),
                keys: keys.clone(),
                key_width: 2,
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 300, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::LowCardinality {
                dict,
                keys: dk,
                key_width,
            } => {
                assert_eq!(key_width, 2);
                assert_eq!(dk, keys);
                match *dict {
                    ColumnData::String(v) => assert_eq!(v, dict_strings),
                    _ => panic!("expected String dict"),
                }
            }
            _ => panic!("expected LowCardinality"),
        }
    }

    #[test]
    fn test_lowcardinality_wire_layout() {
        let col = Column {
            name: "lc".to_string(),
            data_type: "LowCardinality(String)".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::LowCardinality {
                dict: Box::new(ColumnData::String(vec![
                    "".to_string(),
                    "a".to_string(),
                    "b".to_string(),
                ])),
                keys: vec![1, 2, 1],
                key_width: 1,
            },
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();

        let expected_tail: Vec<u8> = {
            let mut v = Vec::new();
            v.extend_from_slice(&1i64.to_le_bytes()); // state prefix
            let metadata: u64 = (1 << 9) | (1 << 10); // HasAdditional + NeedUpdate
            v.extend_from_slice(&metadata.to_le_bytes());
            v.extend_from_slice(&3u64.to_le_bytes()); // dict_size
            v.extend_from_slice(&[0, 1, b'a', 1, b'b']); // dict strings
            v.extend_from_slice(&3u64.to_le_bytes()); // keys_count
            v.extend_from_slice(&[1, 2, 1]); // UInt8 keys
            v
        };
        let tail = &buf[buf.len() - expected_tail.len()..];
        assert_eq!(tail, expected_tail.as_slice());
    }

    #[test]
    fn test_lowcardinality_validate_rejects_bad_key_width() {
        let c = ColumnData::LowCardinality {
            dict: Box::new(ColumnData::String(vec!["".to_string(), "a".to_string()])),
            keys: vec![1],
            key_width: 3,
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_lowcardinality_validate_rejects_oversized_key() {
        let c = ColumnData::LowCardinality {
            dict: Box::new(ColumnData::String(vec!["".to_string(), "a".to_string()])),
            keys: vec![300],
            key_width: 1,
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn test_lowcardinality_validate_rejects_out_of_dict_range() {
        let c = ColumnData::LowCardinality {
            dict: Box::new(ColumnData::String(vec!["".to_string(), "a".to_string()])),
            keys: vec![5],
            key_width: 1,
        };
        let mut buf = Vec::new();
        let err = c.encode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    // -- Phase 8: JSON Tier 1 --

    #[test]
    fn test_json_tier1_roundtrip() {
        let col = Column {
            name: "j".to_string(),
            data_type: "JSON".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Json(vec![
                "{\"a\":1}".to_string(),
                "{}".to_string(),
                "{\"x\":\"y\"}".to_string(),
            ]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 3, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Json(v) => {
                assert_eq!(v, vec!["{\"a\":1}", "{}", "{\"x\":\"y\"}"]);
            }
            _ => panic!("expected Json"),
        }
    }

    #[test]
    fn test_json_tier1_zero_rows() {
        let col = Column {
            name: "j".to_string(),
            data_type: "JSON".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Json(vec![]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = Column::decode(&mut cursor, 0, PROTOCOL).unwrap();
        match decoded.data {
            ColumnData::Json(v) => assert!(v.is_empty()),
            _ => panic!("expected Json"),
        }
    }

    #[test]
    fn test_json_tier1_state_prefix_byte() {
        let col = Column {
            name: "j".to_string(),
            data_type: "JSON".to_string(),
            serialization: Serialization::Default,
            data: ColumnData::Json(vec!["{}".to_string()]),
        };
        let mut buf = Vec::new();
        col.encode(&mut buf, PROTOCOL).unwrap();
        let expected_tail = vec![
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // state prefix Int64 = 1
            0x02, b'{', b'}',
        ];
        let tail = &buf[buf.len() - expected_tail.len()..];
        assert_eq!(tail, expected_tail.as_slice());
    }

    #[test]
    fn test_json_tier1_rejects_other_versions() {
        // Hand-craft wire with version 0.
        let mut buf = Vec::new();
        buf.write_string("j").unwrap();
        buf.write_string("JSON").unwrap();
        buf.write_u8(0).unwrap();
        buf.write_i64(0).unwrap();
        let mut cursor = Cursor::new(buf.as_slice());
        let err = Column::decode(&mut cursor, 1, PROTOCOL).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }
}
