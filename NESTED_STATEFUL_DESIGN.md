# Nested / multi-block stateful decoding — design note

> Status: **design only, not implemented.** This is the one large piece left
> deliberately for hand-implementation. The flat decoder is intact and passes
> everything else; the work here is the architectural change that unlocks the
> remaining failures. Target tests: `tests/differential/nested_stateful.txt`
> (~106 cases).

## The problem

Our column decoder is **flat**: for each column (and each composite element) it
reads *state-prefix-then-data* inline, in one pass. ClickHouse's
`ISerialization` does **not** lay the bytes out that way. Serialization happens
in two separate recursive passes over the whole column tree:

1. `serializeBinaryBulkStatePrefix` — walks the *entire* type tree and writes
   **all** state prefixes first (LowCardinality version, Variant discriminator
   mode, Dynamic FLATTENED header + type names, JSON structure, …).
2. `serializeBinaryBulkWithMultipleStreams` — walks the tree again and writes
   **all** the data.

So for a column like `Tuple(LowCardinality(String), LowCardinality(String))`
the wire is:

```
[LC1 state prefix][LC2 state prefix]   ← all prefixes, batched (pass 1)
[LC1 data][LC2 data]                   ← all data        (pass 2)
```

Our decoder instead does `LC1 prefix → LC1 data → LC2 prefix → LC2 data`. It
reads `LC1 prefix` (ok), then reads the **first bytes of `LC2`'s prefix** where
it expected `LC1`'s per-block metadata — and desyncs. From there it interprets
arbitrary bytes as a dictionary/keys count and (before the allocation guards
added in `read_lc_count`/`checked_count`/`read_string`) tried to allocate
exabytes.

The same shape appears in three guises, all currently failing:

| Guise | Example test | Current symptom |
|-------|--------------|-----------------|
| Versioned type **nested in a composite** (`Tuple`/`Array`/`Map`/`Nullable`) | `02452_check_low_cardinality` (`Tuple(LowCardinality(String)…)`), `02835_nested_array_lowcardinality` | giant-alloc → now a clean "stream misaligned" error |
| **replicated/const over** a versioned type | `00752_low_cardinality_left_array_join` | `replicated declared 1 rows but header said 9` (the LC prefix `1` is misread as the row count) |
| **Multi-block** versioned type (state prefix sent once per query, not per block) | large `SELECT … LowCardinality …` | `LowCardinality state prefix N not supported` on block 2+ |
| Stateful **inside Variant/Dynamic/JSON** (non-leaf) | `02989_variant_comparison`, Dynamic-of-LC | `column type 'Dynamic' not yet supported` / desync |

The sparse-serialization case (`03925_sparse_values_in_substreams_cache_bug`)
is the same root cause: sparse substreams are also part of the batched prefix
walk, and a nested sparse column desyncs a sibling's read.

## What's already in place

- `ColumnData` already models `LowCardinality`, `Variant`, `Dynamic`,
  `JsonObject` for the **leaf, single-block** case — these pass today.
- Allocation guards (`read_lc_count`, `checked_count`, `read_string`'s length
  bound) turn the desync from a process **abort** into a catchable decode
  error. They are a safety net, **not** a fix — the data still isn't decoded.
- `IMPLEMENTATION_NOTES.md` §2.8 documents the single-block state-prefix
  workaround the flat decoder relies on today.

## The required architecture

Mirror `ISerialization`'s two-pass, stateful model. Concretely:

### 1. A per-column deserialization state object

Introduce something like:

```rust
/// Persists across blocks within one query response, per top-level column.
struct DeserializeState {
    // Per node in the type tree, in the SAME order serializeBinaryBulkStatePrefix
    // visits them. e.g. LowCardinality: the global dictionary; Dynamic: version
    // + runtime type names + per-variant sub-states; Variant: discriminator mode.
    ...
}
```

This replaces the assumption "a fresh `Column`/`ColumnData` per block with no
memory." It is the analogue of clickhouse-go's `ReadStatePrefix` /
`WriteStatePrefix` lifecycle living on the column object.

### 2. Split decode into prefix and data passes

```
fn read_state_prefix(r, type, &mut state)      // pass 1: recurse the type tree,
                                               // read every prefix, fill state
fn read_data(r, type, rows, &state) -> ColumnData   // pass 2: recurse again,
                                                     // read data using state
```

Both recurse into composites (`Tuple`/`Array`/`Map`/`Nullable`/`Nested`) and
into `Variant`/`Dynamic`/`JSON` runtime sub-columns **in ClickHouse's exact
visiting order** (depth-first, the order in which the C++ writes them).

### 3. Drive it from `Block::decode`

Today each block builds fresh columns. Change to:

- The query response loop owns a `Vec<DeserializeState>` (one per column),
  created lazily on the **first block with rows > 0** (the header block,
  rows == 0, carries no prefix — keep that rule).
- For that first data block: `read_state_prefix` for every column, then
  `read_data`.
- For subsequent blocks: **skip the prefix** (it was only sent once) and call
  `read_data` with the retained state. This is what fixes the multi-block
  `state prefix N not supported` failures.

### 4. Match ClickHouse's prefix-walk order exactly

The single highest-risk detail. Cross-check against
`ClickHouse/src/DataTypes/Serializations/Serialization*.cpp`
(`serializeBinaryBulkStatePrefix`) for each composite — e.g. `SerializationTuple`
calls each element's `serializeBinaryBulkStatePrefix` in declaration order
*before* any element's data; `SerializationArray` forwards the nested type's
prefix; `SerializationLowCardinality` writes the `KeysSerializationVersion`
once. Get the order wrong and you desync exactly like the flat decoder does now.

## Suggested implementation order (smallest blast radius first)

1. **LowCardinality nested in `Array`/`Tuple`/`Nullable`, single block.**
   Covers `02835`, `02452`, `02155_nested_lc_defalut_bug`, `00800`, etc. Pure
   prefix/data split; no cross-block state yet.
2. **Multi-block LowCardinality.** Add the retained-state-across-blocks path.
3. **replicated/const over LowCardinality** (`00752`, `03393`) — once the LC
   prefix is read in pass 1, the replicated row-count field reads correctly.
4. **Non-leaf `Variant` / `Dynamic` / `JSON`** (stateful runtime sub-types).
   Largest; build on 1–3.
5. **Nested sparse** (`03925`) — sparse substreams in the prefix walk.

## How to verify

```
make up
cargo build --release --bin ch-tsv
JOBS=8 tests/differential/run.sh "$CLICKHOUSE_QUERIES" \
    tests/differential/nested_stateful.txt
```

Track the PASS count up from ~0. Cross-check individual cases against the
server with `clickhouse-client … FORMAT Native | xxd` to see the real
prefix/data byte layout (this is how the leaf cases and the new fixed-width
types in this round were validated).

## Out of scope even after this

- `AggregateFunction(func, …)` — intermediate aggregation **state**; the wire
  format is per-function and would need each aggregate's state (de)serializer.
- `QBit(T, N)` — bit-plane-transposed vector storage; a distinct codec, not a
  state-prefix issue.

Both render fine once *finalized* server-side; only their raw on-wire state is
undecoded. See `NATIVE_FORMAT.md` §3.1.15.
