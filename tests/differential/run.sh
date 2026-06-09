#!/usr/bin/env bash
# Differential harness — parallel execution.
#
# Usage: tests/differential/run.sh <CORPUS_DIR> <ALLOWLIST_FILE> [--verbose]
#
# For each .sql file named in ALLOWLIST_FILE:
#   1. Wraps the SQL in a per-test CREATE/USE/DROP DATABASE envelope so DDL
#      tests don't collide.
#   2. Runs `ch-tsv <wrapped.sql>` capturing stdout, stderr, exit code.
#   3. Diffs stdout against `<CORPUS_DIR>/<base>.reference`.
#   4. Classifies into pass / mismatch / unsupported_type / server_error /
#      io_error / crash and writes the classification to <OUTDIR>/<base>.bucket.
# After all tests finish, aggregates the .bucket files into a summary.
#
# JOBS env var controls parallelism (default 8). Tests run in parallel since
# each has its own ephemeral database — no cross-test state.
#
# TIMEOUT_S caps each test's wall-clock (default 30s). A timed-out test
# lands in the CRASH bucket so a single hung query can't block the run.

set -u -o pipefail

if [[ $# -lt 2 ]]; then
    echo "usage: $0 <CORPUS_DIR> <ALLOWLIST_FILE> [--verbose]" >&2
    exit 64
fi

CORPUS="$1"
LIST="$2"
VERBOSE=0
if [[ $# -ge 3 && "$3" == "--verbose" ]]; then
    VERBOSE=1
fi

BIN="$(cd "$(dirname "$0")/../.." && pwd)/target/release/ch-tsv"
if [[ ! -x "$BIN" ]]; then
    echo "ch-tsv binary missing at $BIN — run cargo build --release --bin ch-tsv" >&2
    exit 1
fi

OUTDIR="$(cd "$(dirname "$0")" && pwd)/out"
mkdir -p "$OUTDIR"

JOBS="${JOBS:-8}"
TIMEOUT_S="${TIMEOUT_S:-30}"
DB_PREFIX="test_$$"

# Per-test worker. Args: relpath, db_suffix.
run_one() {
    local relpath="$1"
    local db_suffix="$2"
    local base="${relpath%.sql}"
    local sql_path="$CORPUS/$relpath"
    local ref_path="$CORPUS/${base}.reference"
    local out_path="$OUTDIR/${base}.actual"
    local err_path="$OUTDIR/${base}.stderr"
    local bucket_path="$OUTDIR/${base}.bucket"
    local db="${DB_PREFIX}_${db_suffix}"

    if [[ ! -f "$ref_path" ]]; then
        echo "MISSING_REF" > "$bucket_path"
        return
    fi

    local tmpfile
    tmpfile=$(mktemp)
    {
        echo "CREATE DATABASE IF NOT EXISTS $db;"
        echo "USE $db;"
        cat "$sql_path"
        echo ";"
        echo "DROP DATABASE IF EXISTS $db;"
    } > "$tmpfile"

    timeout "$TIMEOUT_S" "$BIN" "$tmpfile" > "$out_path" 2> "$err_path"
    local rc=$?
    rm -f "$tmpfile"

    # Normalize the per-test database name back to `default`. The reference
    # files were captured in the canonical `default` database, so DDL echoes
    # (SHOW CREATE TABLE, etc.) embed `default.<table>` while our wrapped run
    # uses an ephemeral `test_<pid>_<seq>` database. This substitution only
    # affects tests that print the db name; all others are untouched.
    if [[ $rc -eq 0 && -s "$out_path" ]]; then
        sed -i "s/${db}/default/g" "$out_path"
    fi

    local bucket
    case $rc in
        0)
            if diff -q "$ref_path" "$out_path" > /dev/null 2>&1; then
                bucket=PASS
            else
                bucket=MISMATCH
            fi
            ;;
        1)   bucket=IO_ERROR ;;
        2)   bucket=SERVER_ERROR ;;
        3)   bucket=UNSUPPORTED_TYPE ;;
        124) bucket=CRASH; echo "ch-tsv: timed out after ${TIMEOUT_S}s" >> "$err_path" ;;
        *)   bucket=CRASH ;;
    esac
    echo "$bucket" > "$bucket_path"
}

export -f run_one
export CORPUS OUTDIR BIN TIMEOUT_S DB_PREFIX

# Hand each (relpath, sequence-number) pair to a parallel worker. The seq
# number gives each test a unique DB name even within the same PID.
grep -v '^#' "$LIST" | grep -v '^$' | nl -ba | \
    xargs -P "$JOBS" -n 2 bash -c 'run_one "$2" "$1"' _

# Aggregate.
declare -i pass=0 mismatch=0 unsupported=0 server_err=0 io_err=0 crash=0 missing_ref=0
declare -a mismatch_files=()
declare -a unsupported_files=()
declare -a server_err_files=()
declare -a io_err_files=()
declare -a crash_files=()
declare -A first_err_msg

while IFS= read -r relpath || [[ -n "$relpath" ]]; do
    [[ -z "$relpath" || "$relpath" =~ ^# ]] && continue
    base="${relpath%.sql}"
    bucket_path="$OUTDIR/${base}.bucket"
    err_path="$OUTDIR/${base}.stderr"
    [[ ! -f "$bucket_path" ]] && continue
    bucket=$(<"$bucket_path")
    case $bucket in
        PASS)              pass+=1 ;;
        MISMATCH)          mismatch+=1; mismatch_files+=("$relpath"); first_err_msg["$relpath"]="output diff" ;;
        UNSUPPORTED_TYPE)  unsupported+=1; unsupported_files+=("$relpath"); first_err_msg["$relpath"]="$(head -n1 "$err_path" 2>/dev/null)" ;;
        SERVER_ERROR)      server_err+=1; server_err_files+=("$relpath"); first_err_msg["$relpath"]="$(head -n1 "$err_path" 2>/dev/null)" ;;
        IO_ERROR)          io_err+=1; io_err_files+=("$relpath"); first_err_msg["$relpath"]="$(head -n1 "$err_path" 2>/dev/null)" ;;
        CRASH)             crash+=1; crash_files+=("$relpath"); first_err_msg["$relpath"]="$(head -n1 "$err_path" 2>/dev/null)" ;;
        MISSING_REF)       missing_ref+=1 ;;
    esac
done < "$LIST"

total=$((pass + mismatch + unsupported + server_err + io_err + crash))

echo "============================================================"
echo "  Differential harness summary (parallel, JOBS=$JOBS)"
echo "============================================================"
printf "  PASS              %d\n" "$pass"
printf "  MISMATCH          %d\n" "$mismatch"
printf "  UNSUPPORTED_TYPE  %d\n" "$unsupported"
printf "  SERVER_ERROR      %d\n" "$server_err"
printf "  IO_ERROR          %d\n" "$io_err"
printf "  CRASH             %d\n" "$crash"
printf "  ------------------------\n"
printf "  TOTAL             %d\n" "$total"
[[ $missing_ref -gt 0 ]] && printf "  (skipped, no .reference: %d)\n" "$missing_ref"
echo

show_bucket() {
    local label="$1"; shift
    local -a files=("$@")
    [[ ${#files[@]} -eq 0 ]] && return 0
    echo "--- $label (first 5) ---"
    local n=0
    for f in "${files[@]}"; do
        local msg="${first_err_msg[$f]:-}"
        printf "  %s\n      %s\n" "$f" "$msg"
        n=$((n + 1))
        [[ $n -ge 5 ]] && break
    done
    echo
}

[[ ${#mismatch_files[@]}    -gt 0 ]] && show_bucket "MISMATCH"          "${mismatch_files[@]}"
[[ ${#unsupported_files[@]} -gt 0 ]] && show_bucket "UNSUPPORTED_TYPE"  "${unsupported_files[@]}"
[[ ${#server_err_files[@]}  -gt 0 ]] && show_bucket "SERVER_ERROR"      "${server_err_files[@]}"
[[ ${#io_err_files[@]}      -gt 0 ]] && show_bucket "IO_ERROR"          "${io_err_files[@]}"
[[ ${#crash_files[@]}       -gt 0 ]] && show_bucket "CRASH"             "${crash_files[@]}"

if [[ $VERBOSE -eq 1 && $mismatch -gt 0 ]]; then
    echo "=== Mismatch diffs (full) ==="
    for f in "${mismatch_files[@]}"; do
        base="${f%.sql}"
        echo "--- $f ---"
        diff -u "$CORPUS/${base}.reference" "$OUTDIR/${base}.actual" | head -40
        echo
    done
fi

# Exit 0 only if every test passed.
if [[ $pass -eq $total ]]; then
    exit 0
else
    exit 1
fi
