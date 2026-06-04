// ch-tsv — differential-testing wrapper.
//
// Reads a SQL file (or `-` for stdin), runs each statement through the
// client, and prints rows in TabSeparated format on stdout. Designed to be
// diffed against ClickHouse's `.reference` files.
//
// Exit codes (must stay stable for the harness):
//   0  ok
//   1  io / connect / protocol error
//   2  server-side exception
//   3  unsupported column type in formatter
//   4  cli / argument error

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::process::ExitCode;

use ch_proto::client::Connection;
use ch_proto::proto::column::ColumnData;
use ch_proto::tsv;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();

    let mut host = String::from("127.0.0.1:9000");
    let mut database: Option<String> = None;
    let mut user: Option<String> = None;
    let mut password: Option<String> = None;
    let mut path: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                let Some(v) = args.get(i + 1) else { return arg_error("--host requires value"); };
                host = v.clone();
                i += 2;
            }
            "--database" => {
                let Some(v) = args.get(i + 1) else { return arg_error("--database requires value"); };
                database = Some(v.clone());
                i += 2;
            }
            "--user" => {
                let Some(v) = args.get(i + 1) else { return arg_error("--user requires value"); };
                user = Some(v.clone());
                i += 2;
            }
            "--password" => {
                let Some(v) = args.get(i + 1) else { return arg_error("--password requires value"); };
                password = Some(v.clone());
                i += 2;
            }
            "-h" | "--help" => {
                print_usage();
                return ExitCode::from(0);
            }
            a if a.starts_with("--") => {
                return arg_error(&format!("unknown option {a}"));
            }
            _ => {
                if path.is_some() {
                    return arg_error("more than one positional argument");
                }
                path = Some(args[i].clone());
                i += 1;
            }
        }
    }

    let Some(path) = path else {
        print_usage();
        return ExitCode::from(4);
    };

    let sql = if path == "-" {
        let mut s = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut s) {
            eprintln!("ch-tsv: stdin read failed: {e}");
            return ExitCode::from(1);
        }
        s
    } else {
        match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ch-tsv: cannot read {path}: {e}");
                return ExitCode::from(1);
            }
        }
    };

    let mut conn = match Connection::connect(
        &host,
        database.as_deref(),
        user.as_deref(),
        password.as_deref(),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ch-tsv: connect to {host}: {e}");
            return ExitCode::from(1);
        }
    };

    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    let statements = parse_statements_with_markers(&sql);
    // Only fetch the error-code map if at least one marker is name-based —
    // saves a startup query for tests with no markers (the common case).
    let need_code_map = statements
        .iter()
        .any(|s| s.expected_errors.iter().any(|e| matches!(e, MarkerCode::Name(_))));
    let code_map: HashMap<String, i32> = if need_code_map {
        match fetch_error_code_map(&mut conn) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("ch-tsv: failed to fetch error-code map: {e}");
                return ExitCode::from(1);
            }
        }
    } else {
        HashMap::new()
    };

    for stmt in statements {
        if stmt.text.trim().is_empty() {
            continue;
        }
        let expected_codes: Vec<i32> = stmt
            .expected_errors
            .iter()
            .filter_map(|c| match c {
                MarkerCode::Number(n) => Some(*n),
                MarkerCode::Name(name) => code_map.get(name.as_str()).copied(),
            })
            .collect();
        let expects_error = !stmt.expected_errors.is_empty();

        match conn.query(&stmt.text) {
            Ok(result) => {
                if expects_error {
                    // Test expected an error but got success — that's a failure.
                    eprintln!(
                        "ch-tsv: expected error {:?} but query succeeded: {}",
                        stmt.expected_errors,
                        first_chars(&stmt.text, 80)
                    );
                    return ExitCode::from(2);
                }
                if let Some(code) = print_result(&mut out, &result) {
                    return code;
                }
            }
            Err(e) => {
                if expects_error {
                    let actual = extract_server_code(&e.to_string());
                    match actual {
                        Some(code) if expected_codes.contains(&code) => {
                            // Expected error — suppress and continue.
                            continue;
                        }
                        Some(code) => {
                            eprintln!(
                                "ch-tsv: expected error {:?} but got code {}: {e}",
                                stmt.expected_errors, code
                            );
                            return ExitCode::from(2);
                        }
                        None => {
                            eprintln!("ch-tsv: query failed (couldn't extract code): {e}");
                            return ExitCode::from(2);
                        }
                    }
                }
                eprintln!("ch-tsv: query failed: {e}");
                return ExitCode::from(2);
            }
        }
    }

    if let Err(e) = out.flush() {
        eprintln!("ch-tsv: stdout flush failed: {e}");
        return ExitCode::from(1);
    }
    ExitCode::from(0)
}

// -- Test-hint marker parsing ------------------------------------------------
//
// ClickHouse stateless tests carry `-- { serverError NAME }` (or numeric)
// comments that flag the statement preceding them as expected-to-fail. The
// canonical parser lives in `ClickHouse/src/Client/TestHint.cpp`; we
// reproduce the subset needed for differential testing.
//
// We don't distinguish serverError from clientError on the wire — both
// surface as the same numeric error code from the server, which matches
// what `clientError` markers in fact target (the client parses the SQL
// first, and at our protocol level "client" still means the server's
// SYNTAX_ERROR-class response).

#[derive(Debug, Clone)]
enum MarkerCode {
    Number(i32),
    Name(String),
}

struct ParsedStatement {
    text: String,
    expected_errors: Vec<MarkerCode>,
}

fn parse_statements_with_markers(sql: &str) -> Vec<ParsedStatement> {
    let raw = split_statements(sql);
    let mut out: Vec<ParsedStatement> = Vec::new();

    for raw_stmt in raw {
        // The split keeps any leading whitespace + comments preceding the
        // next statement's content. Markers found in those leading comments
        // belong to the PRIOR statement (the one that just terminated with `;`).
        let (leading_markers, rest) = extract_leading_markers(&raw_stmt);
        if !leading_markers.is_empty() {
            if let Some(last) = out.last_mut() {
                last.expected_errors.extend(leading_markers);
            }
        }
        // Trailing markers (in the body of the statement, before `;`) belong
        // to THIS statement. Less common but valid.
        let (trailing_markers, cleaned) = extract_inline_markers(rest);
        let text = cleaned.trim().to_string();
        if text.is_empty() && trailing_markers.is_empty() {
            continue;
        }
        out.push(ParsedStatement {
            text,
            expected_errors: trailing_markers,
        });
    }

    out
}

// Walk leading whitespace + comment lines/blocks. For each line comment of
// the form `-- { ... }`, attempt to parse a marker. Stops at the first
// non-comment non-whitespace token. Returns the extracted markers and the
// remaining text.
fn extract_leading_markers(text: &str) -> (Vec<MarkerCode>, &str) {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut markers: Vec<MarkerCode> = Vec::new();

    loop {
        // Skip whitespace.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            let comment = &text[start..i];
            markers.extend(parse_marker_comment(comment));
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            }
            let comment = &text[start..i];
            markers.extend(parse_marker_comment(comment));
        } else {
            break;
        }
    }

    (markers, &text[i..])
}

// Find markers anywhere in the statement body and remove their comment text.
// This handles markers that appear MID-statement or on lines without an
// immediately-preceding `;`.
fn extract_inline_markers(text: &str) -> (Vec<MarkerCode>, String) {
    let mut markers: Vec<MarkerCode> = Vec::new();
    let mut cleaned = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut last_emit = 0;

    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\'' => i = skip_single_quoted(bytes, i),
            b'"' => i = skip_double_quoted(bytes, i),
            b'`' => i = skip_backtick(bytes, i),
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                let comment = &text[start..i];
                let found = parse_marker_comment(comment);
                if !found.is_empty() {
                    cleaned.push_str(&text[last_emit..start]);
                    last_emit = i;
                    markers.extend(found);
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                if i + 1 < bytes.len() {
                    i += 2;
                }
            }
            _ => i += 1,
        }
    }
    cleaned.push_str(&text[last_emit..]);
    (markers, cleaned)
}

// Parse one comment line/block looking for `{ (server|client|)Error <codes> }`.
fn parse_marker_comment(comment: &str) -> Vec<MarkerCode> {
    let open = match comment.find('{') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let close = match comment[open..].find('}') {
        Some(rel) => open + rel,
        None => return Vec::new(),
    };
    let inner = comment[open + 1..close].trim();

    // Strip the keyword prefix.
    let mut rest = inner;
    if let Some(after) = rest.strip_prefix("serverError") {
        rest = after;
    } else if let Some(after) = rest.strip_prefix("clientError") {
        rest = after;
    } else if let Some(after) = rest.strip_prefix("error") {
        // Bare `error` keyword (per the canonical parser, applies to both
        // server and client). Whitespace must follow to avoid matching
        // an identifier that happens to start with "error".
        if after.starts_with(char::is_whitespace) {
            rest = after;
        } else {
            return Vec::new();
        }
    } else {
        return Vec::new();
    }

    rest.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Ok(n) = s.parse::<i32>() {
                MarkerCode::Number(n)
            } else {
                MarkerCode::Name(s.to_string())
            }
        })
        .collect()
}

// Fetch all error code → name mappings from the server. The server's
// `errorCodeToName(N)` returns the empty string for codes that aren't
// registered; we filter those out. ~650 codes total.
fn fetch_error_code_map(conn: &mut Connection) -> io::Result<HashMap<String, i32>> {
    // `errorCodeToName` returns `LowCardinality(String)` natively; CAST to
    // plain String so the per-block decoder hands us a `ColumnData::String`
    // directly instead of the LC dictionary shape.
    let result = conn.query(
        "SELECT toInt32(number), CAST(errorCodeToName(number) AS String) \
         FROM numbers(2000) WHERE errorCodeToName(number) != ''",
    )?;
    let mut map = HashMap::new();
    for block in &result.rows {
        if block.columns.len() != 2 {
            continue;
        }
        let codes = match &block.columns[0].data {
            ColumnData::Int32(v) => v,
            _ => continue,
        };
        let names = match &block.columns[1].data {
            ColumnData::String(v) => v,
            _ => continue,
        };
        for (code, name) in codes.iter().zip(names.iter()) {
            map.insert(name.clone(), *code);
        }
    }
    Ok(map)
}

// Extract the numeric server error code from a query() error message. The
// client formats it as `... ServerException { code: 62, name: "...", ... }`.
// Returns None if no code pattern is present.
fn extract_server_code(msg: &str) -> Option<i32> {
    let key = "code: ";
    let start = msg.find(key)? + key.len();
    let tail = &msg[start..];
    let end = tail.find(|c: char| !c.is_ascii_digit() && c != '-')?;
    tail[..end].parse::<i32>().ok()
}

fn print_result<W: Write>(
    out: &mut W,
    result: &ch_proto::query_result::QueryResult,
) -> Option<ExitCode> {
    for block in &result.rows {
        for row in 0..block.rows_count {
            if let Err(e) = tsv::write_row(out, &block.columns, row) {
                return Some(formatter_or_io_exit(e));
            }
        }
    }
    if let Some(t) = &result.totals {
        if t.rows_count > 0 {
            if out.write_all(b"\n").is_err() {
                return Some(ExitCode::from(1));
            }
            for row in 0..t.rows_count {
                if let Err(e) = tsv::write_row(out, &t.columns, row) {
                    return Some(formatter_or_io_exit(e));
                }
            }
        }
    }
    if let Some(e) = &result.extremes {
        if e.rows_count > 0 {
            if out.write_all(b"\n").is_err() {
                return Some(ExitCode::from(1));
            }
            for row in 0..e.rows_count {
                if let Err(err) = tsv::write_row(out, &e.columns, row) {
                    return Some(formatter_or_io_exit(err));
                }
            }
        }
    }
    None
}

fn first_chars(s: &str, n: usize) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i >= n {
            out.push_str("...");
            break;
        }
        out.push(c);
    }
    out
}

fn formatter_or_io_exit(e: io::Error) -> ExitCode {
    if e.kind() == io::ErrorKind::Unsupported {
        eprintln!("ch-tsv: {e}");
        ExitCode::from(3)
    } else {
        eprintln!("ch-tsv: write failed: {e}");
        ExitCode::from(1)
    }
}

fn arg_error(msg: &str) -> ExitCode {
    eprintln!("ch-tsv: {msg}");
    print_usage();
    ExitCode::from(4)
}

fn print_usage() {
    eprintln!("usage: ch-tsv [--host ADDR] [--database DB] [--user U] [--password P] <FILE|->");
}

// Split SQL on top-level `;`, respecting:
//   '...'    single-quoted strings (with '' as inner-quote escape)
//   "..."    double-quoted identifiers
//   `...`    back-tick identifiers
//   -- ...   line comments to end of line
//   /* ... */ block comments
//
// Backslash escapes are not honored inside ClickHouse single-quoted strings
// when `allow_settings_after_format_in_insert = 0` (the default), but '\'
// is treated as an escape character in many contexts. We honor `\<any>`
// inside quoted strings to be conservative — the only practical effect is
// not splitting on a semicolon that appears immediately after a backslash.
fn split_statements(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\'' => i = skip_single_quoted(bytes, i),
            b'"' => i = skip_double_quoted(bytes, i),
            b'`' => i = skip_backtick(bytes, i),
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => i = skip_line_comment(bytes, i),
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => i = skip_block_comment(bytes, i),
            b';' => {
                out.push(sql[start..i].to_string());
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < sql.len() {
        let tail = &sql[start..];
        if !tail.trim().is_empty() {
            out.push(tail.to_string());
        }
    }
    out
}

fn skip_single_quoted(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'\'' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                } else {
                    return i + 1;
                }
            }
            _ => i += 1,
        }
    }
    i
}

fn skip_double_quoted(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    i
}

fn skip_backtick(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() && bytes[i] != b'`' {
        i += 1;
    }
    if i < bytes.len() {
        i + 1
    } else {
        i
    }
}

fn skip_line_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return i + 2;
        }
        i += 1;
    }
    bytes.len()
}
