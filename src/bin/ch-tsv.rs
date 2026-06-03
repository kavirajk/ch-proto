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

use std::env;
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::process::ExitCode;

use ch_proto::client::Connection;
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

    for stmt in split_statements(&sql) {
        let trimmed = stmt.trim();
        if trimmed.is_empty() {
            continue;
        }
        match conn.query(trimmed) {
            Ok(result) => {
                // Data rows.
                for block in &result.rows {
                    for row in 0..block.rows_count {
                        if let Err(e) = tsv::write_row(&mut out, &block.columns, row) {
                            return formatter_or_io_exit(e);
                        }
                    }
                }
                // WITH TOTALS — single row preceded by a blank line.
                // Matches TabSeparatedRowOutputFormat::writeBeforeTotals().
                if let Some(t) = &result.totals {
                    if t.rows_count > 0 {
                        if out.write_all(b"\n").is_err() {
                            return ExitCode::from(1);
                        }
                        for row in 0..t.rows_count {
                            if let Err(e) = tsv::write_row(&mut out, &t.columns, row) {
                                return formatter_or_io_exit(e);
                            }
                        }
                    }
                }
                // WITH EXTREMES — min row + max row preceded by a blank line.
                // Matches TabSeparatedRowOutputFormat::writeBeforeExtremes().
                if let Some(e) = &result.extremes {
                    if e.rows_count > 0 {
                        if out.write_all(b"\n").is_err() {
                            return ExitCode::from(1);
                        }
                        for row in 0..e.rows_count {
                            if let Err(err) = tsv::write_row(&mut out, &e.columns, row) {
                                return formatter_or_io_exit(err);
                            }
                        }
                    }
                }
            }
            Err(e) => {
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
