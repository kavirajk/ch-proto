#!/usr/bin/env bash
# Stage 0 differential harness.
#
# Usage: tests/differential/run.sh <CORPUS_DIR> <ALLOWLIST_FILE> [--verbose]
#
# For each .sql file named in ALLOWLIST_FILE:
#   1. Runs `ch-tsv <CORPUS_DIR>/<file>.sql` capturing stdout + exit code.
#   2. Diffs the captured stdout against `<CORPUS_DIR>/<file>.reference`.
#   3. Buckets the result into pass / mismatch / unsupported_type /
#      server_error / io_error / crash.
#
# Outputs a summary table and, for the first few failures, a one-line root
# cause hint. With --verbose, also dumps the full unified diff for each
# mismatch.

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

BIN="$(dirname "$0")/../../target/release/ch-tsv"
if [[ ! -x "$BIN" ]]; then
    echo "ch-tsv binary missing at $BIN — run cargo build --release --bin ch-tsv" >&2
    exit 1
fi

OUTDIR="$(dirname "$0")/out"
mkdir -p "$OUTDIR"

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
    sql_path="$CORPUS/$relpath"
    ref_path="$CORPUS/${base}.reference"
    out_path="$OUTDIR/${base}.actual"
    err_path="$OUTDIR/${base}.stderr"

    if [[ ! -f "$ref_path" ]]; then
        missing_ref+=1
        continue
    fi

    "$BIN" "$sql_path" > "$out_path" 2> "$err_path"
    rc=$?

    case $rc in
        0)
            if diff -q "$ref_path" "$out_path" > /dev/null 2>&1; then
                pass+=1
            else
                mismatch+=1
                mismatch_files+=("$relpath")
                first_err_msg["$relpath"]="output diff"
            fi
            ;;
        1)
            io_err+=1
            io_err_files+=("$relpath")
            first_err_msg["$relpath"]="$(head -n1 "$err_path")"
            ;;
        2)
            server_err+=1
            server_err_files+=("$relpath")
            first_err_msg["$relpath"]="$(head -n1 "$err_path")"
            ;;
        3)
            unsupported+=1
            unsupported_files+=("$relpath")
            first_err_msg["$relpath"]="$(head -n1 "$err_path")"
            ;;
        *)
            crash+=1
            crash_files+=("$relpath")
            first_err_msg["$relpath"]="exit $rc; $(head -n1 "$err_path" 2>/dev/null)"
            ;;
    esac
done < "$LIST"

total=$((pass + mismatch + unsupported + server_err + io_err + crash))

echo "============================================================"
echo "  Stage 0 differential summary"
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

# Exit status: 0 only if every test in the list passed.
if [[ $pass -eq $total ]]; then
    exit 0
else
    exit 1
fi
