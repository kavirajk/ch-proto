use std::io::{Error, ErrorKind, Result};

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
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        format!("custom serialization for column '{name}' not yet supported"),
                    ));
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

        let data = ColumnData::decode(r, &data_type, rows)?;
        Ok(Column {
            name,
            data_type,
            serialization,
            data,
        })
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
    Ok(dts)
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
        // Manually encode: name="x", type="LowCardinality(String)" (not yet
        // supported — versioned type, see SPEC §8.4), has_custom=0
        let mut buf = Vec::new();
        buf.write_string("x").unwrap();
        buf.write_string("LowCardinality(String)").unwrap();
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
    fn test_column_rejects_custom_serialization_on_decode() {
        // Server sends has_custom=1, we should reject with Unsupported.
        let mut buf = Vec::new();
        buf.write_string("x").unwrap();
        buf.write_string("UInt8").unwrap();
        buf.write_u8(1).unwrap();

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
}
