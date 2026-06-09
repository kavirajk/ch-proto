#!/usr/bin/env bash
# Classify every non-PASS test in out/ by root cause, so we can separate
# "no value for native protocol/format validation" (environment, dialect,
# version skew, harness artifacts) from genuine protocol/format failures.
#
# Usage: classify.sh <CORPUS_DIR> <LIST_FILE>
# Emits, per test:  <TAG>\t<relpath>
# A trailing summary table is printed to stderr.
set -u -o pipefail

Q="$1"
LIST="$2"
OUT="$(cd "$(dirname "$0")" && pwd)/out"

declare -A count

classify() {
    local rel="$1" base bucket err sql act
    base="${rel%.sql}"
    bucket=$(cat "$OUT/$base.bucket" 2>/dev/null || echo NO_RESULT)
    err="$OUT/$base.stderr"
    sql="$Q/$rel"
    act="$OUT/$base.actual"

    [[ "$bucket" == PASS ]]        && { echo PASS; return; }
    [[ "$bucket" == MISSING_REF ]] && { echo SKIP_NOREF; return; }
    [[ "$bucket" == NO_RESULT ]]   && { echo NO_RESULT; return; }

    local e
    e="$(head -c 4000 "$err" 2>/dev/null)"

    # --- environmental / non-protocol (NO VALUE) ---
    case "$e" in
        *"without ZooKeeper"*|*"ZooKeeper"*)                 echo ENV_ZK; return;;
        *"Requested cluster"*|*"cluster '"*"not found"*)     echo ENV_CLUSTER; return;;
        *"Substitution \`CLICKHOUSE"*|*"CLICKHOUSE_DATABASE"*) echo ENV_SUBST; return;;
        *"KQL"*|*"kusto"*)                                   echo ENV_KQL; return;;
        *"neither a builtin setting"*|*"Unknown setting"*|*"Unknown experimental"*) echo ENV_SETTING; return;;
        *"shutting down due to a fatal error"*)              echo CASCADE_SERVERDOWN; return;;
    esac
    if [[ "$bucket" == IO_ERROR ]]; then
        case "$e" in
            *"fill whole buffer"*|*"connect"*|*"Connection reset"*|*"broken pipe"*) echo CASCADE_IO; return;;
            *"valid UTF-8"*|*"valid utf-8"*) echo ENV_NONUTF8_SQL; return;;   # test file itself isn't UTF-8
        esac
    fi

    # --- genuine client protocol/format gaps & bugs (VALUE) ---
    case "$e" in
        *"Array offsets not monotonic"*|*"replicated decoder"*|*"replicated declared"*) echo CLIENT_DECODE_BUG; return;;
        *"invalid Tuple type string"*)        echo CLIENT_TUPLE_BUG; return;;
        *"LowCardinality state prefix"*)      echo CLIENT_LC_PREFIX; return;;
        *"JSON serialization version"*)       echo CLIENT_JSON_VER; return;;
        *"memory allocation of"*)             echo CLIENT_DECODE_CRASH; return;;
        *"invalid utf-8 sequence"*)           echo CLIENT_UTF8_DECODE; return;;
        *"not yet supported"*)                echo CLIENT_TYPE_UNSUPPORTED; return;;
        *"unsupported column type"*)          echo CLIENT_TSV_FORMATTER; return;;
        *"timed out"*)                        echo TIMEOUT; return;;
    esac

    # --- residual server-side errors: config / version skew / analyzer (NO VALUE) ---
    if [[ "$bucket" == SERVER_ERROR ]]; then
        case "$e" in
            *"graphite_rollup"*|*"system.zookeeper"*|*"No macro"*|*"getMacro"*|*"default_cluster_macro"*) echo ENV_CONFIG; return;;
            *"Transactions are not supported"*|*"Introspection functions"*|*"readonly mode"*|*"allow_introspection"*) echo ENV_CONFIG; return;;
            *"does not exist. In scope"*|*"doesn't match"*|*"Unknown function"*) echo ENV_VERSION_FN; return;;
            *"Multi-statements are not allowed"*|*"Empty query"*) echo ARTIFACT_ENVELOPE; return;;
            *) echo ENV_SERVER_ANALYZER; return;;
        esac
    fi

    if [[ "$bucket" == MISMATCH ]]; then
        # harness artifacts (fixable; not protocol/format)
        if grep -qE 'test_[0-9]+_[0-9]+' "$act" 2>/dev/null; then echo ARTIFACT_DBNAME; return; fi
        if grep -qiE '\{[[:space:]]*echo' "$sql" 2>/dev/null; then echo ARTIFACT_ECHO; return; fi
        echo REAL_FORMAT_MISMATCH; return
    fi

    echo "OTHER_${bucket}"
}

while IFS= read -r rel || [[ -n "$rel" ]]; do
    [[ -z "$rel" || "$rel" =~ ^# ]] && continue
    tag="$(classify "$rel")"
    count[$tag]=$(( ${count[$tag]:-0} + 1 ))
    echo -e "${tag}\t${rel}"
done < "$LIST"

{
    echo "===================== classification summary ====================="
    for k in "${!count[@]}"; do printf "%-26s %d\n" "$k" "${count[$k]}"; done | sort -k2 -rn
} >&2
