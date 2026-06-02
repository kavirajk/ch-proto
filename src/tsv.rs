// TabSeparated formatter for QueryResult column data.
//
// Stage 0 scope: 8/16/32/64-bit integers (signed + unsigned), Float32/64
// with inf/nan special-cases, Bool, String, and Nullable wrapping any of
// the above. Other ColumnData variants return io::ErrorKind::Unsupported
// so the wrapper binary can exit with a distinct status code.
//
// TSV escaping (TabSeparated): backslash, tab, newline, CR, NUL, backspace,
// and form-feed are escaped with a leading backslash. NULL in Nullable
// values renders as the literal two bytes `\N`.

use crate::proto::column::{Column, ColumnData};
use std::io::{self, Write};

pub fn write_row<W: Write>(w: &mut W, columns: &[Column], row: usize) -> io::Result<()> {
    for (i, c) in columns.iter().enumerate() {
        if i > 0 {
            w.write_all(b"\t")?;
        }
        write_value(w, &c.data, row)?;
    }
    w.write_all(b"\n")
}

pub fn write_value<W: Write>(w: &mut W, col: &ColumnData, row: usize) -> io::Result<()> {
    match col {
        ColumnData::Uint8(v) => write!(w, "{}", v[row]),
        ColumnData::Uint16(v) => write!(w, "{}", v[row]),
        ColumnData::Uint32(v) => write!(w, "{}", v[row]),
        ColumnData::Uint64(v) => write!(w, "{}", v[row]),
        ColumnData::Int8(v) => write!(w, "{}", v[row]),
        ColumnData::Int16(v) => write!(w, "{}", v[row]),
        ColumnData::Int32(v) => write!(w, "{}", v[row]),
        ColumnData::Int64(v) => write!(w, "{}", v[row]),
        ColumnData::Bool(v) => w.write_all(if v[row] { b"true" } else { b"false" }),
        ColumnData::Float32(v) => write_f32(w, v[row]),
        ColumnData::Float64(v) => write_f64(w, v[row]),
        ColumnData::String(v) => write_string_escaped(w, v[row].as_bytes()),
        ColumnData::Nullable { inner, nulls } => {
            if nulls[row] == 1 {
                w.write_all(b"\\N")
            } else {
                write_value(w, inner, row)
            }
        }
        other => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("tsv: unsupported column type: {}", variant_name(other)),
        )),
    }
}

fn write_f32<W: Write>(w: &mut W, f: f32) -> io::Result<()> {
    if f.is_nan() {
        w.write_all(b"nan")
    } else if f.is_infinite() {
        w.write_all(if f.is_sign_negative() { b"-inf" } else { b"inf" })
    } else {
        write!(w, "{}", f)
    }
}

fn write_f64<W: Write>(w: &mut W, f: f64) -> io::Result<()> {
    if f.is_nan() {
        w.write_all(b"nan")
    } else if f.is_infinite() {
        w.write_all(if f.is_sign_negative() { b"-inf" } else { b"inf" })
    } else {
        write!(w, "{}", f)
    }
}

fn write_string_escaped<W: Write>(w: &mut W, bytes: &[u8]) -> io::Result<()> {
    // Flush in chunks: walk the bytes, emit a replacement when we hit a
    // special, otherwise extend the current literal run.
    let mut chunk_start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        let escape: &[u8] = match b {
            b'\\' => b"\\\\",
            b'\t' => b"\\t",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            0x00 => b"\\0",
            0x08 => b"\\b",
            0x0c => b"\\f",
            _ => continue,
        };
        if chunk_start < i {
            w.write_all(&bytes[chunk_start..i])?;
        }
        w.write_all(escape)?;
        chunk_start = i + 1;
    }
    if chunk_start < bytes.len() {
        w.write_all(&bytes[chunk_start..])?;
    }
    Ok(())
}

fn variant_name(c: &ColumnData) -> &'static str {
    match c {
        ColumnData::Uint8(_) => "UInt8",
        ColumnData::Uint16(_) => "UInt16",
        ColumnData::Uint32(_) => "UInt32",
        ColumnData::Uint64(_) => "UInt64",
        ColumnData::Int8(_) => "Int8",
        ColumnData::Int16(_) => "Int16",
        ColumnData::Int32(_) => "Int32",
        ColumnData::Int64(_) => "Int64",
        ColumnData::String(_) => "String",
        ColumnData::FixedString { .. } => "FixedString",
        ColumnData::Float32(_) => "Float32",
        ColumnData::Float64(_) => "Float64",
        ColumnData::Bool(_) => "Bool",
        ColumnData::Date(_) => "Date",
        ColumnData::Date32(_) => "Date32",
        ColumnData::DateTime(_) => "DateTime",
        ColumnData::DateTime64 { .. } => "DateTime64",
        ColumnData::Uuid(_) => "UUID",
        ColumnData::Ipv4(_) => "IPv4",
        ColumnData::Ipv6(_) => "IPv6",
        ColumnData::Enum16(_) => "Enum16",
        ColumnData::Decimal32 { .. } => "Decimal32",
        ColumnData::Decimal64 { .. } => "Decimal64",
        ColumnData::Decimal128 { .. } => "Decimal128",
        ColumnData::Decimal256 { .. } => "Decimal256",
        ColumnData::Int128(_) => "Int128",
        ColumnData::Uint128(_) => "UInt128",
        ColumnData::Int256(_) => "Int256",
        ColumnData::Uint256(_) => "UInt256",
        ColumnData::LowCardinality { .. } => "LowCardinality",
        ColumnData::Json(_) => "JSON",
        ColumnData::Nullable { .. } => "Nullable",
        ColumnData::Array { .. } => "Array",
        ColumnData::Tuple(_) => "Tuple",
        ColumnData::Map { .. } => "Map",
        ColumnData::Nested { .. } => "Nested",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val(col: ColumnData) -> String {
        let mut buf = Vec::new();
        write_value(&mut buf, &col, 0).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn ints_render_decimal() {
        assert_eq!(val(ColumnData::Uint8(vec![0])), "0");
        assert_eq!(val(ColumnData::Uint8(vec![255])), "255");
        assert_eq!(val(ColumnData::Int8(vec![-128])), "-128");
        assert_eq!(val(ColumnData::Uint32(vec![4_294_967_295])), "4294967295");
        assert_eq!(val(ColumnData::Int64(vec![-9_223_372_036_854_775_808])), "-9223372036854775808");
        assert_eq!(val(ColumnData::Uint64(vec![18_446_744_073_709_551_615])), "18446744073709551615");
    }

    #[test]
    fn bool_renders_words() {
        assert_eq!(val(ColumnData::Bool(vec![true])), "true");
        assert_eq!(val(ColumnData::Bool(vec![false])), "false");
    }

    #[test]
    fn float_specials() {
        assert_eq!(val(ColumnData::Float32(vec![f32::NAN])), "nan");
        assert_eq!(val(ColumnData::Float32(vec![f32::INFINITY])), "inf");
        assert_eq!(val(ColumnData::Float32(vec![f32::NEG_INFINITY])), "-inf");
        assert_eq!(val(ColumnData::Float64(vec![f64::NAN])), "nan");
        assert_eq!(val(ColumnData::Float64(vec![f64::INFINITY])), "inf");
        assert_eq!(val(ColumnData::Float64(vec![f64::NEG_INFINITY])), "-inf");
    }

    #[test]
    fn float_finite_shortest_roundtrip() {
        // Rust's `{}` for f64 emits the shortest decimal that round-trips,
        // matching ClickHouse's TSV behavior for finite values.
        assert_eq!(val(ColumnData::Float64(vec![1.0])), "1");
        assert_eq!(val(ColumnData::Float64(vec![1.5])), "1.5");
        assert_eq!(val(ColumnData::Float64(vec![-0.0])), "-0");
    }

    #[test]
    fn string_escapes_specials() {
        assert_eq!(val(ColumnData::String(vec!["hello".to_string()])), "hello");
        assert_eq!(val(ColumnData::String(vec!["a\tb".to_string()])), "a\\tb");
        assert_eq!(val(ColumnData::String(vec!["a\nb".to_string()])), "a\\nb");
        assert_eq!(val(ColumnData::String(vec!["a\\b".to_string()])), "a\\\\b");
        assert_eq!(val(ColumnData::String(vec!["a\rb".to_string()])), "a\\rb");
        // NUL embedded in a String is valid in ClickHouse — must be escaped.
        let mut s = String::new();
        s.push_str("a");
        s.push('\0');
        s.push('b');
        assert_eq!(val(ColumnData::String(vec![s])), "a\\0b");
    }

    #[test]
    fn string_passes_unicode_through() {
        assert_eq!(val(ColumnData::String(vec!["日本語".to_string()])), "日本語");
    }

    #[test]
    fn nullable_null_and_present() {
        let mut buf = Vec::new();
        let col = ColumnData::Nullable {
            inner: Box::new(ColumnData::Int32(vec![42, 0])),
            nulls: vec![0, 1],
        };
        write_value(&mut buf, &col, 0).unwrap();
        write_value(&mut buf, &col, 1).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "42\\N");
    }

    #[test]
    fn unsupported_returns_unsupported_kind() {
        let col = ColumnData::Array {
            inner: Box::new(ColumnData::Int32(vec![1, 2, 3])),
            offsets: vec![3],
        };
        let mut buf = Vec::new();
        let err = write_value(&mut buf, &col, 0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("Array"));
    }

    #[test]
    fn write_row_tab_separates_columns() {
        let cols = vec![
            Column {
                name: "a".to_string(),
                data_type: "UInt32".to_string(),
                serialization: crate::proto::column::Serialization::Default,
                data: ColumnData::Uint32(vec![1]),
            },
            Column {
                name: "b".to_string(),
                data_type: "String".to_string(),
                serialization: crate::proto::column::Serialization::Default,
                data: ColumnData::String(vec!["x".to_string()]),
            },
        ];
        let mut buf = Vec::new();
        write_row(&mut buf, &cols, 0).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "1\tx\n");
    }
}
