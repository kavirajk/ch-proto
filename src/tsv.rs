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

use crate::proto::column::{Column, ColumnData, NULL_DISCRIMINATOR};
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
        ColumnData::Date(v) => write_date(w, v[row] as i64),
        ColumnData::Date32(v) => write_date(w, v[row] as i64),
        ColumnData::DateTime(v) => write_datetime(w, v[row] as i64, 0, 0),
        ColumnData::DateTime64 { scale, values } => {
            write_datetime64(w, values[row], *scale)
        }
        ColumnData::FixedString { n, data } => {
            // FixedString: exactly N bytes per row, NUL-padded on the right
            // if the stored string was shorter. C++ uses
            // `getEndWithOptionalTrim` which only trims when
            // `output_format_tsv_only_string_fields` is on — by default no
            // trimming. We follow the default and escape every byte.
            let start = row * *n;
            let end = start + *n;
            write_string_escaped(w, &data[start..end])
        }
        ColumnData::Uuid(v) => {
            // SerializationUUID::serializeTextEscaped writes the canonical
            // 36-char hyphenated form, which is what `Uuid::to_string()`
            // produces.
            write!(w, "{}", v[row])
        }
        ColumnData::Ipv4(v) => {
            // Stored as the network-order u32 with `a` in bits [31:24].
            // See `src/proto/column.rs::ColumnData::Ipv4` doc comment.
            let n = v[row];
            write!(
                w,
                "{}.{}.{}.{}",
                (n >> 24) & 0xff,
                (n >> 16) & 0xff,
                (n >> 8) & 0xff,
                n & 0xff
            )
        }
        ColumnData::Ipv6(v) => {
            // 16 raw network-order bytes; Ipv6Addr::to_string emits the
            // canonical RFC 5952 form (`::1`, `::ffff:1.2.3.4`, etc.).
            let addr = std::net::Ipv6Addr::from(v[row]);
            write!(w, "{}", addr)
        }
        ColumnData::Enum16(v) => {
            // KNOWN STAGE 1 LIMITATION: C++ writes the enum's *label*
            // (`'active'`) rather than its integer. The label lives in the
            // type string and isn't reachable from ColumnData alone — we'd
            // need to thread the data_type through write_value, which is
            // deferred. Output the integer instead, accepting mismatches on
            // tests whose .reference uses the name form.
            write!(w, "{}", v[row])
        }
        ColumnData::Decimal32 { scale, values } => write_decimal(w, values[row] as i128, *scale),
        ColumnData::Decimal64 { scale, values } => write_decimal(w, values[row] as i128, *scale),
        ColumnData::Decimal128 { scale, values } => write_decimal(w, values[row], *scale),
        ColumnData::Int128(v) => write!(w, "{}", v[row]),
        ColumnData::Uint128(v) => write!(w, "{}", v[row]),
        ColumnData::Array { inner, offsets } => write_array(w, inner, offsets, row),
        ColumnData::Tuple(elems) => write_tuple(w, elems, row),
        ColumnData::Map { keys, values, offsets } => write_map(w, keys, values, offsets, row),
        ColumnData::LowCardinality { dict, keys, .. } => {
            // LowCardinality(T) renders as T after dictionary lookup. Keys
            // index into the dict; the dict's value at that index is the
            // logical row's value.
            write_value(w, dict, keys[row] as usize)
        }
        ColumnData::Json(v) => {
            // JSON Tier 1: column stores the JSON text. TSV escape rules
            // are the same as for String (backslash, tab, newline, etc.,
            // plus single quote). Double quotes inside JSON are NOT
            // escaped — they're not in the TSV special-char set.
            write_string_escaped(w, v[row].as_bytes())
        }
        ColumnData::Nullable { inner, nulls } => {
            if nulls[row] == 1 {
                w.write_all(b"\\N")
            } else {
                write_value(w, inner, row)
            }
        }
        ColumnData::Variant {
            discriminators,
            offsets,
            columns,
        } => {
            // A Variant row renders as its active sub-column's value, or \N
            // for the NULL discriminator. The selected sub-column is dense,
            // so we index it by the precomputed per-row offset.
            let d = discriminators[row];
            if d == NULL_DISCRIMINATOR {
                w.write_all(b"\\N")
            } else {
                write_value(w, &columns[d as usize], offsets[row] as usize)
            }
        }
        ColumnData::Dynamic {
            discriminators,
            offsets,
            columns,
            ..
        } => {
            // A Dynamic row renders as its active sub-column's value, or \N
            // for the NULL discriminator (== columns.len(), one past the
            // last type).
            let d = discriminators[row];
            if d as usize >= columns.len() {
                w.write_all(b"\\N")
            } else {
                write_value(w, &columns[d as usize], offsets[row] as usize)
            }
        }
        other => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("tsv: unsupported column type: {}", variant_name(other)),
        )),
    }
}

// Match `writeFloatText` in `ClickHouse/src/IO/WriteHelpers.cpp`. The C++
// fast path emits integer-valued floats as integers (no decimal point) and
// otherwise uses a dragonbox-equivalent shortest-decimal formatter. We
// reproduce that with the `ryu` crate (also dragonbox-equivalent) plus a
// `.0` suffix strip for integer-valued non-exponent forms — ryu's natural
// output for `1.0` is `1.0`; ClickHouse's is `1`.
fn write_f32<W: Write>(w: &mut W, f: f32) -> io::Result<()> {
    if f.is_nan() {
        return w.write_all(b"nan");
    }
    if f.is_infinite() {
        return w.write_all(if f.is_sign_negative() { b"-inf" } else { b"inf" });
    }
    let mut buf = ryu::Buffer::new();
    write_ryu_stripped(w, buf.format(f).as_bytes())
}

fn write_f64<W: Write>(w: &mut W, f: f64) -> io::Result<()> {
    if f.is_nan() {
        return w.write_all(b"nan");
    }
    if f.is_infinite() {
        return w.write_all(if f.is_sign_negative() { b"-inf" } else { b"inf" });
    }
    let mut buf = ryu::Buffer::new();
    write_ryu_stripped(w, buf.format(f).as_bytes())
}

fn write_ryu_stripped<W: Write>(w: &mut W, bytes: &[u8]) -> io::Result<()> {
    let stripped = if bytes.ends_with(b".0") && !bytes.contains(&b'e') && !bytes.contains(&b'E') {
        &bytes[..bytes.len() - 2]
    } else {
        bytes
    };
    w.write_all(stripped)
}

fn write_string_escaped<W: Write>(w: &mut W, bytes: &[u8]) -> io::Result<()> {
    // Matches `writeEscapedString` in `ClickHouse/src/IO/WriteHelpers.h`,
    // which is `writeAnyEscapedString<'\''>` — i.e. the single-quote IS one
    // of the escaped characters (`'` → `\'`).
    write_escaped_bytes_with_quote(w, bytes, b'\'')
}

fn write_escaped_bytes_with_quote<W: Write>(
    w: &mut W,
    bytes: &[u8],
    quote: u8,
) -> io::Result<()> {
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
            _ if b == quote => match quote {
                b'\'' => b"\\'",
                b'"' => b"\\\"",
                _ => continue,
            },
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

// Date / DateTime formatters. The DateTime types are always rendered in
// UTC for Stage 1 — interpreting per-column timezone strings (parsed out
// of the data_type, e.g. `DateTime('America/New_York')`) requires a tz
// database and is deferred.
//
// Matches `writeDateText` and `writeDateTimeText` in
// `ClickHouse/src/IO/WriteHelpers.h` for the default `Simple` format
// (`YYYY-MM-DD` and `YYYY-MM-DD HH:MM:SS[.fraction]`).

const SECONDS_PER_DAY: i64 = 86_400;

fn write_date<W: Write>(w: &mut W, days_since_epoch: i64) -> io::Result<()> {
    let (y, m, d) = civil_from_days(days_since_epoch);
    if y < 0 {
        write!(w, "-{:04}-{:02}-{:02}", -y, m, d)
    } else {
        write!(w, "{:04}-{:02}-{:02}", y, m, d)
    }
}

fn write_datetime<W: Write>(w: &mut W, seconds: i64, fraction: u64, scale: u8) -> io::Result<()> {
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let secs_of_day = seconds.rem_euclid(SECONDS_PER_DAY);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let sec = secs_of_day % 60;
    let (y, m, d) = civil_from_days(days);
    if y < 0 {
        write!(
            w,
            "-{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            -y, m, d, hour, minute, sec
        )?;
    } else {
        write!(
            w,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            y, m, d, hour, minute, sec
        )?;
    }
    if scale > 0 {
        write!(w, ".{:0width$}", fraction, width = scale as usize)?;
    }
    Ok(())
}

fn write_datetime64<W: Write>(w: &mut W, ticks: i64, scale: u8) -> io::Result<()> {
    let factor = 10i64.pow(scale as u32);
    let seconds = ticks.div_euclid(factor);
    let fraction = ticks.rem_euclid(factor) as u64;
    write_datetime(w, seconds, fraction, scale)
}

// Decimal formatter. Matches `writeText(Decimal<T>, scale, ostr,
// trailing_zeros=false)` in `ClickHouse/src/IO/WriteHelpers.h`. Whole and
// fractional parts share a sign — write the leading `-` once (even when the
// whole part rounds to 0), then unsigned magnitude. Trailing zeros in the
// fractional part are trimmed; if the entire fraction is zero, the decimal
// point is omitted. Decimal256 (32-byte two's-complement values) is not
// supported in Stage 1 — it would need 256-bit integer math.
fn write_decimal<W: Write>(w: &mut W, value: i128, scale: u8) -> io::Result<()> {
    let is_neg = value < 0;
    let abs: u128 = value.unsigned_abs();
    let factor: u128 = 10u128.pow(scale as u32);
    let whole = abs / factor;
    let frac = abs % factor;

    if is_neg {
        w.write_all(b"-")?;
    }
    write!(w, "{}", whole)?;

    if scale > 0 && frac != 0 {
        // Pad to `scale` digits, then trim trailing zeros.
        let frac_str = format!("{:0width$}", frac, width = scale as usize);
        let trimmed = frac_str.trim_end_matches('0');
        // If frac != 0, at least one non-zero digit remains.
        w.write_all(b".")?;
        w.write_all(trimmed.as_bytes())?;
    }
    Ok(())
}

// Howard Hinnant's `civil_from_days` algorithm. Given a count of days since
// 1970-01-01 (negative for earlier dates), return (year, month, day) in the
// proleptic Gregorian calendar. Domain: any i64 days, output years in i64.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y_no_adjust = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = y_no_adjust + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

// Quoted-form writer for use INSIDE composites (Array/Tuple/Map/Nested).
//
// Matches `serializeTextQuoted` on each leaf type, per
// `SerializationArray::serializeText` calling `nested->serializeTextQuoted`
// on inner elements. Key differences from `write_value` (TextEscaped):
// - Strings/FixedStrings wrap in single quotes with backslash escapes.
// - Dates/DateTimes wrap in single quotes around their YYYY-MM-DD form.
// - UUIDs / IPv4 / IPv6 wrap in single quotes around canonical form.
// - Nullable null renders as the literal text `NULL`, not `\N`.
// - Numbers, decimals: identical to escaped form (no quotes).
fn write_value_quoted<W: Write>(w: &mut W, col: &ColumnData, row: usize) -> io::Result<()> {
    match col {
        // Numbers — no quotes, identical to escaped.
        ColumnData::Uint8(_)
        | ColumnData::Uint16(_)
        | ColumnData::Uint32(_)
        | ColumnData::Uint64(_)
        | ColumnData::Int8(_)
        | ColumnData::Int16(_)
        | ColumnData::Int32(_)
        | ColumnData::Int64(_)
        | ColumnData::Int128(_)
        | ColumnData::Uint128(_)
        | ColumnData::Float32(_)
        | ColumnData::Float64(_)
        | ColumnData::Decimal32 { .. }
        | ColumnData::Decimal64 { .. }
        | ColumnData::Decimal128 { .. }
        | ColumnData::Enum16(_)
        | ColumnData::Bool(_) => write_value(w, col, row),
        // Strings: 'wrapped with backslash escapes including \''.
        ColumnData::String(v) => {
            w.write_all(b"'")?;
            write_string_escaped(w, v[row].as_bytes())?;
            w.write_all(b"'")
        }
        ColumnData::FixedString { n, data } => {
            let start = row * *n;
            let end = start + *n;
            w.write_all(b"'")?;
            write_string_escaped(w, &data[start..end])?;
            w.write_all(b"'")
        }
        // Dates / times: 'YYYY-MM-DD...' single-quoted.
        ColumnData::Date(v) => {
            w.write_all(b"'")?;
            write_date(w, v[row] as i64)?;
            w.write_all(b"'")
        }
        ColumnData::Date32(v) => {
            w.write_all(b"'")?;
            write_date(w, v[row] as i64)?;
            w.write_all(b"'")
        }
        ColumnData::DateTime(v) => {
            w.write_all(b"'")?;
            write_datetime(w, v[row] as i64, 0, 0)?;
            w.write_all(b"'")
        }
        ColumnData::DateTime64 { scale, values } => {
            w.write_all(b"'")?;
            write_datetime64(w, values[row], *scale)?;
            w.write_all(b"'")
        }
        // Address-like values: quoted canonical form.
        ColumnData::Uuid(v) => write!(w, "'{}'", v[row]),
        ColumnData::Ipv4(v) => {
            let n = v[row];
            write!(
                w,
                "'{}.{}.{}.{}'",
                (n >> 24) & 0xff,
                (n >> 16) & 0xff,
                (n >> 8) & 0xff,
                n & 0xff
            )
        }
        ColumnData::Ipv6(v) => {
            let addr = std::net::Ipv6Addr::from(v[row]);
            write!(w, "'{}'", addr)
        }
        // Nullable inner: literal NULL when null, otherwise recurse.
        ColumnData::Nullable { inner, nulls } => {
            if nulls[row] == 1 {
                w.write_all(b"NULL")
            } else {
                write_value_quoted(w, inner, row)
            }
        }
        // Composites recurse with the quoted form.
        ColumnData::Array { inner, offsets } => write_array(w, inner, offsets, row),
        ColumnData::Tuple(elems) => write_tuple(w, elems, row),
        ColumnData::Map { keys, values, offsets } => write_map(w, keys, values, offsets, row),
        ColumnData::LowCardinality { dict, keys, .. } => {
            write_value_quoted(w, dict, keys[row] as usize)
        }
        other => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("tsv (quoted): unsupported column type: {}", variant_name(other)),
        )),
    }
}

fn write_array<W: Write>(
    w: &mut W,
    inner: &ColumnData,
    offsets: &[u64],
    row: usize,
) -> io::Result<()> {
    let start = if row == 0 { 0 } else { offsets[row - 1] as usize };
    let end = offsets[row] as usize;
    w.write_all(b"[")?;
    for i in start..end {
        if i > start {
            w.write_all(b",")?;
        }
        write_value_quoted(w, inner, i)?;
    }
    w.write_all(b"]")
}

fn write_tuple<W: Write>(w: &mut W, elems: &[ColumnData], row: usize) -> io::Result<()> {
    w.write_all(b"(")?;
    for (i, e) in elems.iter().enumerate() {
        if i > 0 {
            w.write_all(b",")?;
        }
        write_value_quoted(w, e, row)?;
    }
    w.write_all(b")")
}

fn write_map<W: Write>(
    w: &mut W,
    keys: &ColumnData,
    values: &ColumnData,
    offsets: &[u64],
    row: usize,
) -> io::Result<()> {
    let start = if row == 0 { 0 } else { offsets[row - 1] as usize };
    let end = offsets[row] as usize;
    w.write_all(b"{")?;
    for i in start..end {
        if i > start {
            w.write_all(b",")?;
        }
        write_value_quoted(w, keys, i)?;
        w.write_all(b":")?;
        write_value_quoted(w, values, i)?;
    }
    w.write_all(b"}")
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
        ColumnData::Nothing(_) => "Nothing",
        ColumnData::Variant { .. } => "Variant",
        ColumnData::Dynamic { .. } => "Dynamic",
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
        // Integer-valued floats render without a decimal point (C++ takes the
        // itoa fast path here).
        assert_eq!(val(ColumnData::Float64(vec![1.0])), "1");
        assert_eq!(val(ColumnData::Float64(vec![1.5])), "1.5");
        assert_eq!(val(ColumnData::Float64(vec![-0.0])), "-0");
        assert_eq!(val(ColumnData::Float64(vec![100.0])), "100");
    }

    #[test]
    fn float_extreme_magnitudes_use_scientific() {
        // The whole reason we depend on ryu: Rust's stdlib `Display` produces
        // a 300+ character decimal expansion for these. ClickHouse uses the
        // shortest representation, which is scientific for extreme exponents.
        assert_eq!(val(ColumnData::Float64(vec![1e308])), "1e308");
        assert_eq!(val(ColumnData::Float64(vec![-1e-307])), "-1e-307");
        assert_eq!(val(ColumnData::Float64(vec![1e-302])), "1e-302");
        // The exact value from the .reference of test 00031.
        assert_eq!(
            val(ColumnData::Float64(vec![-8.98846567431158e307])),
            "-8.98846567431158e307"
        );
        assert_eq!(
            val(ColumnData::Float64(vec![-2.2250738585072014e-308])),
            "-2.2250738585072014e-308"
        );
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
    fn date_known_values() {
        // 1970-01-01 is day 0 by definition.
        assert_eq!(val(ColumnData::Date(vec![0])), "1970-01-01");
        assert_eq!(val(ColumnData::Date(vec![1])), "1970-01-02");
        assert_eq!(val(ColumnData::Date(vec![365])), "1971-01-01");
        // Day 11016 = 2000-02-29 (leap-day in a century year — 2000 is /400
        // so it IS a leap year). 30 years × 365 = 10950 + 7 leap days (72,
        // 76, 80, 84, 88, 92, 96 — NOT 2000 itself yet) + 59 (jan 31 + feb 28)
        // = 11016. Day 11017 is 2000-03-01.
        assert_eq!(val(ColumnData::Date(vec![11016])), "2000-02-29");
        assert_eq!(val(ColumnData::Date(vec![11017])), "2000-03-01");
    }

    #[test]
    fn date32_negative_days() {
        // 1969-12-31 is day -1 from 1970-01-01.
        assert_eq!(val(ColumnData::Date32(vec![-1])), "1969-12-31");
        // 1900-01-01: 70 years × 365 = 25550 + 17 leap days (1904, 08, 12,
        // ..., 1968 — 17 of them; 1900 itself is NOT leap because /100 but
        // not /400). Days from 1900-01-01 to 1970-01-01 = 25567. Negate.
        assert_eq!(val(ColumnData::Date32(vec![-25567])), "1900-01-01");
    }

    #[test]
    fn datetime_known_value() {
        // 1970-01-01 00:00:00 UTC = 0 seconds.
        assert_eq!(val(ColumnData::DateTime(vec![0])), "1970-01-01 00:00:00");
        // 2024-01-01 00:00:00 UTC: 54 years × 365 = 19710 + 13 leap days
        // (1972, 76, ..., 2020) = 19723 days; ×86400 = 1_704_067_200.
        assert_eq!(
            val(ColumnData::DateTime(vec![1_704_067_200])),
            "2024-01-01 00:00:00"
        );
        // Same date + 13:45:30 = +49_530 s = 1_704_116_730.
        assert_eq!(
            val(ColumnData::DateTime(vec![1_704_116_730])),
            "2024-01-01 13:45:30"
        );
    }

    #[test]
    fn datetime64_with_scale() {
        // scale 3 (ms): 2024-01-01 13:45:30.125 = 1_704_116_730_125 ticks.
        assert_eq!(
            val(ColumnData::DateTime64 {
                scale: 3,
                values: vec![1_704_116_730_125]
            }),
            "2024-01-01 13:45:30.125"
        );
        // scale 0 — no fractional part.
        assert_eq!(
            val(ColumnData::DateTime64 {
                scale: 0,
                values: vec![1_704_116_730]
            }),
            "2024-01-01 13:45:30"
        );
        // scale 9 — zero-padding for a small fraction.
        assert_eq!(
            val(ColumnData::DateTime64 {
                scale: 9,
                values: vec![1_704_116_730_000_000_001]
            }),
            "2024-01-01 13:45:30.000000001"
        );
    }

    #[test]
    fn unsupported_returns_unsupported_kind() {
        // `Nested` is one of the variants still uncovered in Stage 1.
        let col = ColumnData::Nested {
            fields: vec![],
            offsets: vec![0],
        };
        let mut buf = Vec::new();
        let err = write_value(&mut buf, &col, 0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("Nested"));
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
