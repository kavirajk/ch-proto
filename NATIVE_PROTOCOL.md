# ClickHouse Native Protocol

This specification describes the binary, connection-oriented protocol used over TCP between ClickHouse clients and servers. The protocol carries SQL queries, results, INSERT data, telemetry, and error signals.

This document describes only the protocol — packet framing, the connection state machine, and the bodies of non-Block messages. The bytes inside Data-family packets (Block, Column, type encoding) are described in `NATIVE_FORMAT.md`. Implementation gotchas, historical quirks, and reference-client status notes are in `IMPLEMENTATION_NOTES.md`.

The protocol is binary, packet-based, and stateful. Each TCP connection processes one query at a time; there is no multiplexing.

## Table of Contents

1. [Overview](#1-overview)
2. [Security](#2-security)
3. [Versioning & Feature Gates](#3-versioning--feature-gates)
4. [Packet Envelope](#4-packet-envelope)
5. [Connection Lifecycle](#5-connection-lifecycle)
6. [Message Reference](#6-message-reference)
7. [Packet Type Reference](#7-packet-type-reference)
8. [Configuration](#8-configuration)
9. [Glossary](#9-glossary)

---

## 1. Overview

| Property              | Value |
|-----------------------|-------|
| Transport             | TCP, optionally with TLS |
| Byte order            | Little-endian for fixed-width integers |
| Encoding              | Binary, positional (no field tags except in BlockInfo) |
| Connection model      | Stateful, single query at a time, no multiplexing |
| Versioning            | Negotiated at handshake; features gated by version |
| Data format           | Native format for tabular data — see `NATIVE_FORMAT.md` |

Each message on the wire begins with a VarUInt packet type code, followed by the message body. The format of the body depends on the packet type and the negotiated protocol version.

The native TCP protocol always uses the Native data format on the wire, regardless of any `FORMAT` clause in the SQL. Re-formatting into RowBinary, CSV, JSON, etc. is the client's responsibility, performed after decoding the Native blocks. The HTTP interface is a separate code path that does honour the `FORMAT` clause; HTTP is out of scope for this document.

---

## 2. Security

### 2.1 Transport Security (TLS)

TLS is handled at the transport layer, below the protocol. When TLS is enabled, the entire TCP stream is encrypted. The protocol messages themselves are identical whether TLS is used or not.

### 2.2 Authentication

Authentication occurs during the handshake via the ClientHello message (§6.1). The `user` and `password` fields are sent as plaintext strings. Transport-level encryption (TLS) is expected to protect these credentials in transit.

SSH challenge-response authentication is available at protocol version 54466+ (not specified in this document).

### 2.3 Inter-Server Secret

For distributed query execution, servers authenticate to each other using a shared secret string sent in the Query message (`cluster_secret` field, §6.7). This is gated by `INTERSERVER_SECRET` (v54441). External clients always send an empty string.

---

## 3. Versioning & Feature Gates

### 3.1 Version Negotiation

Both client and server declare their maximum supported protocol version during the handshake. The **negotiated version** is the minimum of the two:

```
negotiated_version = min(client_version, server_version)
```

All subsequent messages on the connection use the negotiated version to determine which fields are present on the wire.

### 3.2 Feature Gates

A feature is identified by the protocol version in which it was introduced. A feature is **active** if the negotiated protocol version is greater than or equal to the feature's version number.

When a feature is active, its associated fields **must** be present on the wire. The protocol is strictly positional — omitting a feature-gated field corrupts the byte stream for all subsequent fields.

### 3.3 Feature Table

| Feature                         | Version | Affects                | Wire impact |
|---------------------------------|---------|------------------------|-------------|
| BLOCK_INFO                      | 51903   | Block                  | Adds the BlockInfo prefix (`is_overflows`, `bucket_number`) to every Block. |
| TIMEZONE                        | 54058   | ServerHello            | Adds the `timezone` field to ServerHello. |
| QUOTA_KEY_IN_CLIENT_INFO        | 54060   | ClientInfo             | Adds the `quota_key` field to ClientInfo. |
| DISPLAY_NAME                    | 54372   | ServerHello            | Adds the `display_name` field to ServerHello. |
| VERSION_PATCH                   | 54401   | ServerHello, ClientInfo | Adds the `version_patch` field to both. |
| SERVER_LOGS                     | 54406   | Log                    | Server emits Log packets when `send_logs_level` is set. |
| WRITE_CLIENT_INFO               | 54420   | Query, Progress        | Adds the ClientInfo block to Query; adds `wrote_rows` and `wrote_bytes` to Progress. |
| SETTINGS_SERIALIZED_AS_STRINGS  | 54429   | Query                  | Encodes settings as string key-value pairs in the Query body. |
| INTERSERVER_SECRET              | 54441   | Query                  | Adds the `cluster_secret` field to Query. |
| OPEN_TELEMETRY                  | 54442   | ClientInfo             | Adds the OpenTelemetry trace context to ClientInfo. |
| DISTRIBUTED_DEPTH               | 54448   | ClientInfo             | Adds the `distributed_depth` field to ClientInfo. |
| INITIAL_QUERY_START_TIME        | 54449   | ClientInfo             | Adds the `initial_time` field (Int64, fixed-width). |
| PROFILE_EVENTS                  | 54451   | ProfileEvents          | Server emits ProfileEvents packets during query execution. |
| PARALLEL_REPLICAS               | 54453   | ClientInfo             | Adds parallel-replica coordination fields to ClientInfo. |
| CUSTOM_SERIALIZATION            | 54454   | Block (Column)         | Adds the `has_custom_serialization` byte after each column's type string. |
| ADDENDUM                        | 54458   | Handshake              | Client sends an addendum (`quota_key`) after the handshake exchange. |
| PARAMETERS                      | 54459   | Query                  | Adds the parameters list to the Query body. |
| SERVER_QUERY_TIME_IN_PROGRESS   | 54460   | Progress               | Adds the `elapsed_ns` field to Progress. |
| PASSWORD_COMPLEXITY_RULES       | 54461   | ServerHello            | Adds a list of password-policy regex patterns and human-readable messages to ServerHello. |
| INTERSERVER_SECRET_V2           | 54462   | ServerHello            | Adds an 8-byte `UInt64` nonce to ServerHello. Used by inter-server query signing; external clients decode and ignore. |
| TOTAL_BYTES_IN_PROGRESS         | 54463   | Progress               | Adds the `total_bytes_to_read` (VarUInt) field to Progress, between `total_rows` and `wrote_rows`. |
| TIMEZONE_UPDATES                | 54464   | TimezoneUpdate         | Adds the `TimezoneUpdate` server packet (type 17). Body: single `String` carrying the new session timezone. Sent when `SET session_timezone` mutates the session-default tz mid-query. |
| SPARSE_SERIALIZATION            | 54465   | Block (Column)         | Server may set `has_custom_serialization = 1` and emit a sparse-encoded column. Wire format: 1-byte kind (0x01 = SPARSE), then VarUInt offset stream terminated by EOG, then the non-default values densely encoded in the inner type. See `NATIVE_FORMAT.md` §2.3.1. |
| SSH_AUTHENTICATION              | 54466   | Auth flow              | Adds SSH challenge-response authentication. Opt-in: client sends a `user` of the form `" SSH KEY AUTHENTICATION " + <real_user>` with empty password to trigger it. See §6.20. |
| TABLE_READ_ONLY_CHECK           | 54467   | TablesStatusResponse   | Adds an `is_readonly` flag to each table's row in TablesStatusResponse. External clients that don't issue `TablesStatusRequest` see no wire change. |
| SYSTEM_KEYWORDS_TABLE           | 54468   | system tables          | Server populates `system.keywords` so the canonical `clickhouse-client` can autocomplete keywords. No native-protocol wire change. |
| ROWS_BEFORE_AGGREGATION         | 54469   | ProfileInfo            | Adds `applied_aggregation` (Bool) and `rows_before_aggregation` (VarUInt) to ProfileInfo, in that order at the tail. |

---

## 4. Packet Envelope

Every message on the wire follows the same outer structure:

```
[VarUInt: packet_type_code]    always encoded as VarUInt
[message body]                  format depends on packet_type_code
```

This applies to both directions (client → server and server → client). Complete packet type code tables are in §7.

The packet type is a VarUInt, not a fixed-width byte. For values < 128 a VarUInt produces the same single byte, but implementations must use VarUInt encoding to remain compatible if future packet types reach ≥ 128.

Message tables in §6 document only the **body** of each packet (the bytes after the packet type code). Field numbering starts at 1 for the first body field.

---

## 5. Connection Lifecycle

A connection is in exactly one of four states at any time: `HANDSHAKE`, `READY`, `READING_RESPONSE`, or terminated. The protocol does not multiplex — a client that sends a new request before draining the previous response interleaves bytes on the wire and corrupts the stream.

### 5.1 States

```
  [Connect]
      |
      v
  HANDSHAKE ---- error ----> [Disconnect]
      |
      ok
      |
      v
  READY <----------------------------------+
      |                                     |
      |--- Ping -------> Pong ----------->--|
      |                                     |
      |--- Query ------> READING_RESPONSE ->|
      |                                     |
      +-------------------------------------+
```

| State              | Description |
|--------------------|-------------|
| `HANDSHAKE`        | Initial state after TCP connection. Only handshake messages (§5.2) are valid. Transitions to `READY` on success or terminates on failure. |
| `READY`            | Idle. Client may send Ping (§5.3), Query (§5.4), or close. The connection may remain in `READY` indefinitely (subject to `idle_connection_timeout`, §8.1.3). |
| `READING_RESPONSE` | Entered when the client sends a Query. The client must fully drain the server's response stream before returning to `READY`. The only allowed client → server packet is Cancel (not specified here). |
| Terminated         | Connection is no longer usable. Cannot be re-used; the client must establish a new TCP connection and restart the handshake. |

### 5.2 Handshake Phase

Authenticates and negotiates the protocol version. Happens exactly once per connection.

**Precondition.** TCP connection just established. No messages exchanged yet.

**Flow.**

```
Client                              Server
  |--- ClientHello ------------------>|
  |<--- ServerHello ------------------|    or Exception
  |                                   |
  |    (compute negotiated_version)   |
  |                                   |
  |--- Addendum --------------------->|    only if negotiated_version ≥ 54458
```

**Steps.**

1. Client sends ClientHello (§6.1) with its maximum supported protocol version.
2. Client reads response. Dispatch by packet type:

   | Packet type           | Action |
   |-----------------------|--------|
   | `Hello` (0) → §6.2    | Decode ServerHello. Compute `negotiated_version = min(client_ver, server_ver)`. Proceed to step 3. |
   | `Exception` (2) → §6.6 | Decode Exception. Return as error. Terminate connection. |
   | anything else         | Protocol violation. Terminate connection. |

3. If `negotiated_version ≥ 54458` (feature `ADDENDUM`), the client sends an Addendum (§6.3). The decision is based on the **negotiated** version, not the client's declared version.

**Postcondition.** On success, connection transitions to `READY`. On any error, connection terminates.

### 5.3 Ping Phase

An application-level keepalive / liveness check. Independent of TCP keepalive. A successful Ping/Pong round-trip confirms the TCP connection is alive in both directions and the server is responsive.

Ping is stateless and uncorrelated with any query. Multiple sequential Pings are independent.

**Precondition.** Connection in `READY`.

**Flow.**

```
Client                              Server
  |--- Ping (0x04) ------------------>|
  |<--- Pong (0x04) ------------------|
```

**Steps.**

1. Client sends Ping (§6.4).
2. Client reads response:

   | Packet type           | Action |
   |-----------------------|--------|
   | `Pong` (4) → §6.5     | Keepalive confirmed. Transition to `READY`. |
   | `Exception` (2) → §6.6 | Decode, return as error. |
   | anything else         | Protocol violation. |

**Postcondition.** Connection returns to `READY`.

### 5.4 Query Phase

The client submits a SQL statement and the server streams back result blocks and execution telemetry. The response is a sequence of packets terminated by exactly one `EndOfStream` or `Exception`.

**Precondition.** Connection in `READY`.

**Flow.**

```
Client                              Server
  |--- Query ------------------------>|     §6.7
  |--- ExternalTable (data) --------->|     §6.11  (optional, for temp tables)
  |--- Empty Data marker ------------>|     §6.11  (required, end-of-client-data)
  |                                    |
  |<--- Data (header block) ----------|     schema: N cols, 0 rows
  |<--- Progress ---------------------|     0 or more, interleaved
  |<--- Log --------------------------|     0 or more (if logs enabled)
  |<--- Data (result block) ----------|     0 or more: N cols, M rows each
  |<--- Totals / Extremes ------------|     0 or more (aggregation queries)
  |<--- ProfileInfo / ProfileEvents --|     0 or more (profiling)
  |<--- Data (empty block) -----------|     boundary marker
  |<--- Progress ---------------------|     final updates
  |<--- EndOfStream ------------------|     authoritative end of query
```

On error at any point:

```
  |<--- Exception ------------------->|     terminates the query
```

**Steps.**

1. Client sends Query (§6.7) with a unique `query_id` (typically a UUID).
2. Client sends external tables (§6.11), then the empty Data marker. The empty Data packet has `table_name = ""`, `num_columns = 0`, `num_rows = 0`. The server does not begin executing the query until it receives this marker.
3. Client transitions to `READING_RESPONSE` and flushes its write buffer.
4. Client reads response packets in a loop. Dispatch by packet type:

   | Packet type           | Action |
   |-----------------------|--------|
   | `Data` (1) → §6.11    | Decode the block. First Data = schema header. Subsequent = result blocks (accumulate). Empty block = boundary marker. `num_rows == 0` is **not** end-of-query. |
   | `Progress` (3) → §6.12 | Execution metrics. Cumulative; not deltas. |
   | `EndOfStream` (5)     | Query complete. Exit the loop. Transition to `READY`. |
   | `ProfileInfo` (6) → §6.13 | Post-execution profiling data. |
   | `Totals` (7) → §6.14  | Aggregation totals block (same wire format as Data). |
   | `Extremes` (8) → §6.15 | Min/max values block (same wire format as Data). |
   | `Log` (10) → §6.16    | Server log line. |
   | `TableColumns` (11) → §6.18 | Column defaults metadata. |
   | `ProfileEvents` (14) → §6.17 | Performance counters. |
   | `Exception` (2) → §6.6 | Decode and return as error. Exit the loop. Transition to `READY`. |
   | anything else         | Unexpected during query phase. Terminate connection. |

**Postcondition.** On `EndOfStream` or handled `Exception`, connection returns to `READY`. On protocol violation or I/O error, connection terminates.

### 5.5 INSERT Phase

A variant of the Query Phase with two extra exchanges. The client submits an INSERT statement; the server responds with a **schema block** describing the target table; the client streams Data packets carrying the rows; the client sends the empty Data marker; the server finishes with `EndOfStream` or `Exception`.

**Precondition.** Connection in `READY`. The SQL is an INSERT statement of the form `INSERT INTO <table> [(<cols>)] VALUES` — no inline `VALUES (...)` literal; the row data flows via Data packets.

**Flow.**

```
Client                                  Server
[Query packet — INSERT body]          → 
[ExternalTable*, then empty terminator] →
                                        ← [optional metadata: TableColumns, Progress, ...]
                                        ← [Data packet: schema block (rows = 0)]
[Data packet: rows N]                 →
[Data packet: rows M]                 →   (additional blocks, optional)
[Data packet: empty block (rows 0)]   →   (end-of-input terminator)
                                        ← [optional Progress, ProfileInfo, Log, ProfileEvents]
                                        ← [EndOfStream]
```

**Steps.**

1. Client sends Query (§6.7) with `body` = the INSERT SQL.
2. Client sends external tables (rare for INSERT) followed by the empty terminator.
3. Client drains metadata packets (TableColumns, Progress, ProfileInfo, Log, ProfileEvents) until it reads the schema Data packet — a Block with 0 rows but full column structure (names + types). The schema block is the contract: the rows the client subsequently sends must match these column shapes.
4. Client sends data block(s). For each block:
   1. Write `VarUInt(ClientPacket::Data = 2)`.
   2. Write `String("")` for the (empty) external-table name.
   3. Encode the Block. Column types must align with the schema block's columns by position.
5. Client sends the end-of-input terminator: a Data packet with an empty Block (0 columns, 0 rows).
6. Client drains the response stream until `EndOfStream` (success) or `Exception` (failure).

**Postcondition.** Connection returns to `READY` on `EndOfStream` or handled `Exception`. Protocol violations or I/O errors terminate the connection.

---

## 6. Message Reference

Fields are listed in wire order. The `Type` column uses:

- `VarUInt` — variable-length unsigned integer (§4.1 of `NATIVE_FORMAT.md`).
- `String` — VarUInt-prefixed UTF-8 bytes (§1.3 of `NATIVE_FORMAT.md`).
- `UInt8`, `Int32`, etc. — fixed-width little-endian integers.
- `Bool` — single byte, 0x00 or 0x01.

The `Role` column indicates who uses each field:

- **client** — set by external clients.
- **inter-server** — meaningful only for server-to-server communication; external clients write a default value.
- **universal** — used by both.

Message tables document only the body of each packet (after the packet type code of §4).

### 6.1 ClientHello (packet type 0)

**Direction.** Client → Server. **Sent when.** First message after TCP connection.

| # | Field            | Type    | Role      | Description |
|---|------------------|---------|-----------|-------------|
| 1 | client_name      | String  | universal | Client identifier (e.g., `"clickhouse-client"`) |
| 2 | version_major    | VarUInt | universal | Client major version |
| 3 | version_minor    | VarUInt | universal | Client minor version |
| 4 | protocol_version | VarUInt | universal | Client's max supported protocol version |
| 5 | database         | String  | universal | Default database name |
| 6 | user             | String  | universal | Username for authentication |
| 7 | password         | String  | universal | Password (plaintext) |

### 6.2 ServerHello (packet type 0)

**Direction.** Server → Client. **Sent when.** Response to ClientHello on successful authentication.

| # | Field            | Type    | Role      | Condition              | Description |
|---|------------------|---------|-----------|------------------------|-------------|
| 1 | server_name      | String  | universal | always                 | Server identifier |
| 2 | version_major    | VarUInt | universal | always                 | Server major version |
| 3 | version_minor    | VarUInt | universal | always                 | Server minor version |
| 4 | protocol_version | VarUInt | universal | always                 | Server's protocol version |
| 5 | timezone         | String  | universal | TIMEZONE (v54058)      | Server timezone (e.g., `"UTC"`) |
| 6 | display_name     | String  | universal | DISPLAY_NAME (v54372)  | Human-readable server name |
| 7 | version_patch    | VarUInt | universal | VERSION_PATCH (v54401) | Server patch version |
| 8 | password_complexity_rules | Rule[] | universal | PASSWORD_COMPLEXITY_RULES (v54461) | Server's password policy. `VarUInt count` followed by `count × Rule`. See below. |
| 9 | nonce            | UInt64  | inter-server | INTERSERVER_SECRET_V2 (v54462) | 8-byte LE random nonce. The server's inter-server query-signing scheme uses it. External clients MUST decode it (to keep the stream aligned) and SHOULD ignore the value. |

**Rule** — element of `password_complexity_rules`:

| # | Field   | Type   | Description |
|---|---------|--------|-------------|
| 1 | pattern | String | Regular-expression pattern that a compliant password must match. |
| 2 | message | String | Human-readable explanation shown when a password fails this rule. |

The list reflects the server operator's password-policy configuration and is purely advisory — the server does not enforce these rules during this handshake. Clients that expose password change/set functionality may use the rules to surface errors before round-tripping a non-compliant password to the server.

To bound resource use against a hostile or misconfigured server, the decoded `count` SHOULD be capped at 256 entries, and each `pattern` and `message` String SHOULD be capped at 4096 bytes. A `count` of `0` (and therefore no following pairs) is the common case for servers with no password policy configured.

### 6.3 Addendum (no packet type)

**Direction.** Client → Server. **Feature gate.** ADDENDUM (v54458). **Sent when.** Immediately after the handshake exchange completes.

Not a distinct packet type — sent as raw fields with no packet type byte prefix.

| # | Field     | Type   | Role         | Description |
|---|-----------|--------|--------------|-------------|
| 1 | quota_key | String | inter-server | Resource quota identifier. External clients send empty string. |

### 6.4 Ping (packet type 4)

**Direction.** Client → Server.

No body. The packet is a single byte `0x04` on the wire.

### 6.5 Pong (packet type 4)

**Direction.** Server → Client.

No body. The packet is a single byte `0x04` on the wire.

### 6.6 Exception (packet type 2)

**Direction.** Server → Client. **Sent when.** Server encounters an error during any phase.

| # | Field       | Type   | Role      | Description |
|---|-------------|--------|-----------|-------------|
| 1 | code        | Int32  | universal | Error code |
| 2 | name        | String | universal | Exception class (e.g., `"DB::Exception"`) |
| 3 | message     | String | universal | Human-readable error message |
| 4 | stack_trace | String | universal | Server-side stack trace |
| 5 | has_nested  | Bool   | universal | If true, another Exception follows immediately |

If `has_nested` is true, another Exception structure follows (without a packet type prefix), forming a chain of nested exceptions.

### 6.7 Query (packet type 1)

**Direction.** Client → Server.

| # | Field          | Type        | Role         | Condition                                | Description |
|---|----------------|-------------|--------------|------------------------------------------|-------------|
| 1 | query_id       | String      | universal    | always                                   | Unique query identifier (UUID) |
| 2 | client_info    | ClientInfo  | universal    | WRITE_CLIENT_INFO (v54420)               | See §6.8 |
| 3 | settings       | Setting[]   | universal    | SETTINGS_SERIALIZED_AS_STRINGS (v54429)  | See §6.9. Terminated by empty key. |
| 4 | cluster_secret | String      | inter-server | INTERSERVER_SECRET (v54441)              | Cluster auth. External clients send empty string. |
| 5 | stage          | VarUInt     | universal    | always                                   | 0 = FetchColumns, 1 = WithMergeableState, 2 = Complete |
| 6 | compression    | VarUInt     | universal    | always                                   | 0 = disabled, 1 = enabled |
| 7 | query_body     | String      | universal    | always                                   | SQL text |
| 8 | parameters     | Parameter[] | client       | PARAMETERS (v54459)                      | See §6.10. Terminated by empty key. |

### 6.8 ClientInfo (embedded in Query)

**Direction.** Client → Server (embedded in Query body, field 2). **Feature gate.** WRITE_CLIENT_INFO (v54420).

| #  | Field                        | Type    | Role         | Condition                              | Description |
|----|------------------------------|---------|--------------|----------------------------------------|-------------|
| 1  | query_kind                   | UInt8   | universal    | always                                 | 0 = NoQuery, 1 = InitialQuery, 2 = SecondaryQuery. External clients send `1`. |
| 2  | initial_user                 | String  | universal    | always                                 | User who initiated the query |
| 3  | initial_query_id             | String  | universal    | always                                 | Original query ID |
| 4  | initial_address              | String  | universal    | always                                 | Originating client socket address in `host:port` format |
| 5  | initial_time                 | Int64   | client       | INITIAL_QUERY_START_TIME (v54449)      | Query start time (microseconds). Fixed-width 8 bytes, not VarUInt |
| 6  | query_interface              | UInt8   | universal    | always                                 | 1 = TCP, 2 = HTTP |
| 7  | os_user                      | String  | client       | if interface = TCP                     | OS username |
| 8  | client_hostname              | String  | client       | if interface = TCP                     | Client machine hostname |
| 9  | client_name                  | String  | client       | if interface = TCP                     | Client application name |
| 10 | version_major                | VarUInt | universal    | if interface = TCP                     | Client major version |
| 11 | version_minor                | VarUInt | universal    | if interface = TCP                     | Client minor version |
| 12 | protocol_version             | VarUInt | universal    | if interface = TCP                     | Negotiated protocol version |
| 13 | quota_key                    | String  | inter-server | QUOTA_KEY_IN_CLIENT_INFO (v54060)      | Resource quota key. External clients send empty string. |
| 14 | distributed_depth            | VarUInt | inter-server | DISTRIBUTED_DEPTH (v54448)             | Distributed query nesting depth. External clients send `0`. |
| 15 | version_patch                | VarUInt | universal    | VERSION_PATCH (v54401), TCP only       | Client patch version |
| 16 | open_telemetry               | (below) | client       | OPEN_TELEMETRY (v54442)                | Trace context. Clients without tracing send `0`. |
| 17 | collaborate_with_initiator   | VarUInt | inter-server | PARALLEL_REPLICAS (v54453)             | Bool as VarUInt. External clients send `0`. |
| 18 | count_participating_replicas | VarUInt | inter-server | PARALLEL_REPLICAS (v54453)             | External clients send `0`. |
| 19 | number_of_current_replica    | VarUInt | inter-server | PARALLEL_REPLICAS (v54453)             | External clients send `0`. |

**OpenTelemetry encoding** (field 16):

```
[UInt8: has_trace]              0 = no trace data follows, 1 = trace data follows
If has_trace == 1:
  [16 bytes: trace_id]          byte-swapped per-8-bytes
  [8 bytes:  span_id]           byte-swapped
  [String:   trace_state]       W3C trace state
  [UInt8:    trace_flags]       W3C trace flags
```

### 6.9 Setting

Encoded inline in the Query body settings list (§6.7, field 3). The list is terminated by a Setting with an empty key (single VarUInt `0`, no flags or value follow).

| # | Field | Type    | Role      | Description |
|---|-------|---------|-----------|-------------|
| 1 | key   | String  | universal | Setting name. Empty = end of list. |
| 2 | flags | VarUInt | universal | Bit flags: `0x01` = Important, `0x02` = Custom, `0x04` = Obsolete |
| 3 | value | String  | universal | Setting value as string |

Fields 2 and 3 are not present when `key` is empty.

### 6.10 Parameter

Query parameters (for parameterized queries like `SELECT {x:UInt64}`). Encoded identically to a Setting with the `Custom` flag (`0x02`) set. Terminated by empty key, same as settings.

| # | Field | Type    | Role   | Description |
|---|-------|---------|--------|-------------|
| 1 | key   | String  | client | Parameter name. Empty = end of list. |
| 2 | flags | VarUInt | client | Always `0x02` (Custom) |
| 3 | value | String  | client | Parameter value as string. See Implementation Notes §1.9 for the single-quoting requirement. |

### 6.11 Data (packet type 1 server→client, packet type 2 client→server)

**Direction.** Both. **Sent when.** Result blocks, INSERT data, external tables, and end-of-data markers.

The wire format is symmetric — both directions include a `table_name` prefix before the Block. The only difference is the packet type byte.

```
[VarUInt: packet_type]     1 (server→client) or 2 (client→server)
[String:  table_name]      External table name; empty in most cases
[Block]                    See NATIVE_FORMAT.md §2 for the Block layout
```

| Field      | Type   | Role      | Description |
|------------|--------|-----------|-------------|
| table_name | String | universal | External table name. Client: empty = end-of-data marker. Server: always empty for query results. |
| Block body | —      | —         | See `NATIVE_FORMAT.md` §2. |

Block variants and their meaning are documented in `NATIVE_FORMAT.md` §2.4.

### 6.12 Progress (packet type 3)

**Direction.** Server → Client. **Sent when.** Periodically during query execution.

All fields are VarUInt. Each Progress packet carries cumulative totals (not deltas) since the start of the query.

| # | Field       | Type    | Role      | Condition                              | Description |
|---|-------------|---------|-----------|----------------------------------------|-------------|
| 1 | rows        | VarUInt | universal | always                                 | Rows processed so far |
| 2 | bytes       | VarUInt | universal | always                                 | Bytes processed so far |
| 3 | total_rows  | VarUInt | universal | always                                 | Estimated total rows (may be 0) |
| 4 | total_bytes | VarUInt | universal | TOTAL_BYTES_IN_PROGRESS (v54463)       | Estimated total bytes (may be 0). Sits BETWEEN `total_rows` and `wrote_rows` on the wire. |
| 5 | wrote_rows  | VarUInt | universal | WRITE_CLIENT_INFO (v54420)             | Rows written (for INSERT) |
| 6 | wrote_bytes | VarUInt | universal | WRITE_CLIENT_INFO (v54420)             | Bytes written (for INSERT) |
| 7 | elapsed_ns  | VarUInt | universal | SERVER_QUERY_TIME_IN_PROGRESS (v54460) | Elapsed nanoseconds since query start |

### 6.13 ProfileInfo (packet type 6)

**Direction.** Server → Client. **Sent when.** Once per query, near the end of execution.

| # | Field                         | Type    | Role      | Condition                          | Description |
|---|-------------------------------|---------|-----------|------------------------------------|-------------|
| 1 | rows                          | VarUInt | universal | always                             | Total rows processed |
| 2 | blocks                        | VarUInt | universal | always                             | Total blocks processed |
| 3 | bytes                         | VarUInt | universal | always                             | Total bytes processed |
| 4 | applied_limit                 | Bool    | universal | always                             | Whether a LIMIT clause was applied |
| 5 | rows_before_limit             | VarUInt | universal | always                             | Row count before LIMIT |
| 6 | calculated_rows_before_limit  | Bool    | universal | always                             | Whether `rows_before_limit` was computed |
| 7 | applied_aggregation           | Bool    | universal | ROWS_BEFORE_AGGREGATION (v54469)   | Whether GROUP BY was applied |
| 8 | rows_before_aggregation       | VarUInt | universal | ROWS_BEFORE_AGGREGATION (v54469)   | Row count before aggregation |

### 6.14 Totals (packet type 7)

**Direction.** Server → Client. **Sent when.** Queries with `WITH TOTALS`.

Wire format: identical to Data (§6.11). A `table_name` string (always empty) followed by a Block. Only the packet type byte differs.

```
[VarUInt: 7]                packet type
[String:  table_name]       always empty
[Block]                     see NATIVE_FORMAT.md §2
```

### 6.15 Extremes (packet type 8)

**Direction.** Server → Client. **Sent when.** The `extremes` setting is enabled.

Wire format: identical to Data (§6.11). The block has exactly 2 rows: row 0 holds the minimum of each column, row 1 holds the maximum.

```
[VarUInt: 8]                packet type
[String:  table_name]       always empty
[Block]                     num_rows = 2
```

### 6.16 Log (packet type 10)

**Direction.** Server → Client. **Sent when.** The query has an active logs queue (`send_logs_level` setting; §8.2.2).

Same envelope and body format as Data (§6.11). The block has fixed `num_columns = 8` and a predefined column schema. Each log line is one row across all 8 columns. A single Log packet may carry many rows.

```
[VarUInt: 10]               packet type
[String:  table_name]       always empty
[Block]                     num_columns = 8, num_rows = number of log lines
```

The 8 columns, in this exact order:

| # | Name                    | Type     | Description |
|---|-------------------------|----------|-------------|
| 1 | event_time              | DateTime | Event timestamp (seconds since epoch) |
| 2 | event_time_microseconds | UInt32   | Microseconds component |
| 3 | host_name               | String   | Server hostname emitting the log |
| 4 | query_id                | String   | Query ID the log belongs to |
| 5 | thread_id               | UInt64   | OS thread ID |
| 6 | priority                | Int8     | Log level (Poco priority: 1 = Fatal, … 8 = Trace) |
| 7 | source                  | String   | Logger name |
| 8 | text                    | String   | Log message text |

### 6.17 ProfileEvents (packet type 14)

**Direction.** Server → Client. **Sent when.** Server emits per-query performance counters.

Same envelope and body format as Data (§6.11). The block has fixed `num_columns = 6` and a predefined schema. Each event is one row.

```
[VarUInt: 14]               packet type
[String:  table_name]       always empty
[Block]                     num_columns = 6, num_rows = number of events
```

The 6 columns:

| # | Name         | Type     | Description |
|---|--------------|----------|-------------|
| 1 | host_name    | String   | Server hostname |
| 2 | current_time | DateTime | Event timestamp |
| 3 | thread_id    | UInt64   | Thread ID |
| 4 | type         | Int8     | Event type: 1 = Increment (counter), 2 = Gauge |
| 5 | name         | String   | Event name (e.g., `"Query"`, `"NetworkReceiveBytes"`) |
| 6 | value        | Int64 or UInt64 | Counter value or gauge reading. Type varies per packet — see Implementation Notes §2.5. |

### 6.18 TableColumns (packet type 11)

**Direction.** Server → Client. **Sent when.** Client needs column-default metadata, typically before INSERTs that omit some columns.

| # | Field               | Type   | Role      | Description |
|---|---------------------|--------|-----------|-------------|
| 1 | external_table      | String | universal | External table name. Empty = main table. |
| 2 | columns_description | String | universal | Textual column definitions, e.g., `"id Int32, name String DEFAULT ''"`. Free-form text — parse as a string. |

### 6.19 TimezoneUpdate (packet type 17)

**Direction.** Server → Client. **Feature gate.** `TIMEZONE_UPDATES` (v54464). **Sent when.** The session-default timezone changes mid-query (e.g., a `SET session_timezone = '...'` runs as part of the query and the server wants the client to know about the new default for formatting subsequent `DateTime` values).

| # | Field    | Type   | Role      | Description |
|---|----------|--------|-----------|-------------|
| 1 | timezone | String | universal | The new session-default timezone (e.g., `"UTC"`, `"Europe/Berlin"`). |

The packet may arrive at any point in the query response stream, between Data / Progress / Log packets. A decoder that ignores `TimezoneUpdate` MUST still consume the trailing `String` to keep the wire aligned.

### 6.20 SSH challenge-response authentication (packet types 11, 12, 18)

**Feature gate.** `SSH_AUTHENTICATION` (v54466). **Opt-in only.** A connection enters the SSH flow when ClientHello sends `user = " SSH KEY AUTHENTICATION " + <real_user>` (with the leading and trailing spaces) and `password = ""`. The server reads the prefix, strips it to recover the real user, and switches to challenge-response.

| Packet | Code | Direction | Body |
|--------|------|-----------|------|
| SSHChallengeRequest | 11 | Client → Server | (no body) |
| SSHChallenge       | 18 | Server → Client | `String challenge` — the bytes to sign |
| SSHChallengeResponse | 12 | Client → Server | `String signature` — the SSH-signed challenge |

The flow runs in place of password authentication, after ClientHello:

1. Client sends ClientHello with the SSH marker prefix.
2. Server replies with ServerHello as usual.
3. Client sends `SSHChallengeRequest` (packet 11).
4. Server replies with `SSHChallenge` carrying random bytes (packet 18).
5. Client signs the bytes with its SSH private key and sends `SSHChallengeResponse` (packet 12) with the signature.
6. Server verifies the signature against the user's registered public key, then continues as if password auth had succeeded (or returns an Exception on failure).

External clients that don't use SSH auth never see packets 11, 12, or 18 — they're entirely off the wire unless the user explicitly opts in via the username prefix.

---

## 7. Packet Type Reference

### 7.1 Client → Server

| Code | Name                      | Body format         | Description |
|------|---------------------------|---------------------|-------------|
| 0    | Hello                     | §6.1                | Handshake initiation |
| 1    | Query                     | §6.7                | Query execution request |
| 2    | Data                      | §6.11               | Data block (INSERT data, external tables, end-of-data marker) |
| 3    | Cancel                    | (no body)           | Cancel running query |
| 4    | Ping                      | §6.4 (no body)      | Keepalive check |
| 5    | TablesStatusRequest       | not specified       | Table status check |
| 6    | KeepAlive                 | not specified       | Connection keepalive |
| 7    | Scalar                    | not specified       | Scalar data block |
| 8    | IgnoredPartUUIDs          | not specified       | Parts to exclude from query |
| 9    | ReadTaskResponse          | not specified       | S3 cluster read response |
| 10   | MergeTreeReadTaskResponse | not specified       | Parallel read task response |
| 11   | SSHChallengeRequest       | not specified       | SSH auth challenge request |
| 12   | SSHChallengeResponse      | not specified       | SSH auth challenge response |
| 13   | QueryPlan                 | not specified       | Query plan |

### 7.2 Server → Client

| Code | Name                           | Body format         | Description |
|------|--------------------------------|---------------------|-------------|
| 0    | Hello                          | §6.2                | Handshake response |
| 1    | Data                           | §6.11               | Result data block |
| 2    | Exception                      | §6.6                | Error |
| 3    | Progress                       | §6.12               | Query execution progress |
| 4    | Pong                           | §6.5 (no body)      | Keepalive response |
| 5    | EndOfStream                    | (no body)           | Query complete |
| 6    | ProfileInfo                    | §6.13               | Post-execution profiling data |
| 7    | Totals                         | §6.14               | GROUP BY WITH TOTALS row |
| 8    | Extremes                       | §6.15               | Min/max values (2-row block) |
| 9    | TablesStatusResponse           | not specified       | Table status response |
| 10   | Log                            | §6.16               | Query execution log lines |
| 11   | TableColumns                   | §6.18               | Column descriptions for defaults |
| 12   | PartUUIDs                      | not specified       | Unique part IDs |
| 13   | ReadTaskRequest                | not specified       | Cluster read task request |
| 14   | ProfileEvents                  | §6.17               | Performance counters |
| 15   | MergeTreeAllRangesAnnouncement | not specified       | Parallel read initialization |
| 16   | MergeTreeReadTaskRequest       | not specified       | Parallel read task assignment |
| 17   | TimezoneUpdate                 | not specified       | Server timezone update |
| 18   | SSHChallenge                   | not specified       | SSH auth challenge |

---

## 8. Configuration

This section documents the tunables that shape native protocol connections.

- **§8.1 Transport-layer settings** — TCP socket options and timeouts. Affect how the TCP connection itself behaves.
- **§8.2 Application-layer settings** — per-query tunables included in the Query packet's `settings` list (§6.9). Affect what the server sends on the wire or how it's framed.
- **§8.3 Settings out of scope** — settings commonly confused with protocol settings but actually controlling SQL execution or storage.

Defaults below reflect the reference server implementation. Values may differ across server versions and deployments.

### 8.1 Transport-Layer Settings

#### 8.1.1 Socket options

| Option               | Default                          | Side       | Description |
|----------------------|----------------------------------|------------|-------------|
| `TCP_NODELAY`        | on                               | both       | Nagle's algorithm disabled. Small packets are sent immediately. |
| `SO_KEEPALIVE`       | on (client), OS default (server) | asymmetric | Kernel-level TCP keepalive probes. Client explicitly enables this when `tcp_keep_alive_timeout > 0`. Server inherits OS default. |
| `SO_RCVBUF` / `SO_SNDBUF` | OS defaults                 | —          | Socket buffer sizes. Not tuned by the protocol. |

#### 8.1.2 Timeouts

| Setting                                  | Default | Unit         | Side   | Description |
|------------------------------------------|---------|--------------|--------|-------------|
| `connect_timeout`                        | 10      | seconds      | client | Timeout for establishing the initial TCP connection. |
| `handshake_timeout_ms`                   | 10000   | milliseconds | client | Timeout for receiving ServerHello during handshake. |
| `send_timeout`                           | 300     | seconds      | both   | If no bytes can be written within this interval, the connection throws. |
| `receive_timeout`                        | 300     | seconds      | both   | If no bytes can be read within this interval, the connection throws. |
| `tcp_keep_alive_timeout`                 | 290     | seconds      | client | Idle duration before the OS sends the first TCP keepalive probe. |
| `receive_data_timeout_ms`                | 2000    | milliseconds | client | Timeout for receiving the first Data packet from a replica. |
| `connect_timeout_with_failover_ms`       | 1000    | milliseconds | client | Per-attempt connect timeout when iterating replicas. |
| `connect_timeout_with_failover_secure_ms`| 1000    | milliseconds | client | Per-attempt connect timeout when iterating replicas over TLS. |
| `hedged_connection_timeout_ms`           | 50      | milliseconds | client | Per-attempt connect timeout for hedged requests. |
| `poll_interval`                          | 10      | seconds      | server | Granularity of the server's idle-connection and shutdown check loop. |

**Timing relationship.**

```
tcp_keep_alive_timeout (290s)
      < receive_timeout (300s)
      < idle_connection_timeout (3600s)
      < tcp_close_connection_after_queries_seconds (0 = unlimited by default)
```

OS keepalive fires first and may detect dead peers silently at the kernel level. Application receive timeout is the next line of defence. Idle timeout is the last resort that reaps long-unused connections.

#### 8.1.3 Connection limits

| Setting                                       | Default       | Unit    | Side   | Description |
|-----------------------------------------------|---------------|---------|--------|-------------|
| `max_connections`                             | 4096          | count   | server | Maximum concurrent TCP connections. |
| `idle_connection_timeout`                     | 3600          | seconds | server | Maximum time an idle connection may remain open. |
| `tcp_close_connection_after_queries_num`      | 0 (unlimited) | count   | server | Maximum number of queries per connection before forced close. |
| `tcp_close_connection_after_queries_seconds`  | 0 (unlimited) | seconds | server | Maximum total connection lifetime regardless of activity. |

A connection that issues queries regularly can live indefinitely. Only idle connections are reaped after 1 hour. There is no default maximum lifetime.

### 8.2 Application-Layer Settings

These settings are carried per-query in the Query packet's `settings` list. They change what the server sends on the wire or how it's framed.

#### 8.2.1 Compression

| Setting                          | Default  | Unit   | Description |
|----------------------------------|----------|--------|-------------|
| `network_compression_method`     | `"LZ4"`  | string | Compression codec when the Query packet's `compression` flag is set. Values: `"LZ4"`, `"LZ4HC"`, `"ZSTD"`, `"NONE"`. |
| `network_zstd_compression_level` | 1        | 1–15   | ZSTD level when `network_compression_method == "ZSTD"`. |

The `compression` flag in the Query packet itself (§6.7 field 6) toggles compression on/off. These settings select which codec is used when it's on.

#### 8.2.2 Log streaming

| Setting                  | Default       | Unit   | Description |
|--------------------------|---------------|--------|-------------|
| `send_logs_level`        | `"fatal"`     | string | Minimum log level. Values: `"none"`, `"fatal"`, `"error"`, `"warning"`, `"information"`, `"debug"`, `"trace"`. |
| `send_logs_source_regexp`| `""`          | string | Regex filter on the logger source. Empty = all sources pass. |

Setting `send_logs_level` to anything other than `"none"` causes the server to emit `Log` packets during query execution.

#### 8.2.3 Progress reporting

| Setting             | Default | Unit         | Description |
|---------------------|---------|--------------|-------------|
| `interactive_delay` | 100000  | microseconds | Target minimum interval between consecutive Progress packets. |

`interactive_delay` is the target minimum, not a strict maximum. The server may send Progress packets less frequently if the query is not producing work fast enough.

#### 8.2.4 Result envelope

| Setting                | Default       | Unit               | Description |
|------------------------|---------------|--------------------|-------------|
| `extremes`             | false         | bool               | When true, server sends an Extremes packet (§6.15) with min/max values per column. |
| `max_result_rows`      | 0 (unlimited) | count              | Cap on rows transmitted. Behaviour controlled by `result_overflow_mode`. |
| `max_result_bytes`     | 0 (unlimited) | uncompressed bytes | Cap on uncompressed byte volume. Behaviour controlled by `result_overflow_mode`. |
| `result_overflow_mode` | `"throw"`     | string             | `"throw"` ends the stream with Exception; `"break"` sends partial results followed by EndOfStream. |

#### 8.2.5 Async INSERT

| Setting                          | Default | Unit    | Description |
|----------------------------------|---------|---------|-------------|
| `async_insert`                   | true    | bool    | When true, INSERT data is queued server-side and batched. |
| `wait_for_async_insert`          | true    | bool    | When true (with `async_insert` on), the server holds the response until queued data is flushed. |
| `wait_for_async_insert_timeout`  | 120     | seconds | Maximum time the server waits for flush before returning. |

#### 8.2.6 Distributed tracing

| Setting                                 | Default | Unit              | Description |
|-----------------------------------------|---------|-------------------|-------------|
| `opentelemetry_start_trace_probability` | 0.0     | 0–1 probability   | Server-side probability of attaching OpenTelemetry context to response telemetry. |

### 8.3 Settings Out of Scope

These settings are commonly confused with protocol-level settings but actually control SQL execution, storage, or CPU use — not wire behaviour. A protocol implementation does not need to handle them specially.

- `max_threads` — parallelism within query execution.
- `max_memory_usage` — per-query memory cap.
- `max_block_size`, `preferred_block_size_bytes` — internal block sizing during query processing; wire blocks are independent of these.
- `compile_expressions` — JIT compilation; CPU-only.
- `async_insert_max_data_size` — server-side queue buffer.
- All settings prefixed `input_format_*` and `output_format_*` — apply to non-native formats over HTTP, not the native protocol.

### 8.4 Settings Not Covered

The chunked protocol (negotiated via the addendum at v54470+) introduces additional transport tunables not specified here:

- `proto_send_chunked`, `proto_recv_chunked` — negotiated mode (`chunked`, `notchunked`, `chunked_optional`, `notchunked_optional`).
- Chunk framing, length prefixes, chunk-level flow control.

---

## 9. Glossary

**Cancel** — a client-initiated packet (type 3) that aborts a running query. Not specified in detail in this document.

**End-of-client-data marker** — an empty Data packet (0 columns, 0 rows) sent by the client after the Query packet (and any external tables) to signal "no more input." The server does not begin executing the query until it receives this marker.

**Feature** — a wire-format change introduced in a specific protocol version. Active when the negotiated version is at or above the feature's version. See §3.

**Inter-server** — a role label indicating a field that is only meaningful for server-to-server communication in distributed queries. External clients write a default value (typically empty string, 0, or false).

**Negotiated version** — `min(client_version, server_version)`, computed during the handshake. Determines which features are active for the lifetime of the connection.

**Packet** — a wire message: a VarUInt packet type code followed by a body whose format depends on the type. See §4.

**Packet type code** — the leading VarUInt of a packet that identifies its format. Values 0–18 are currently assigned. Tables in §7.

**Response stream** — the sequence of packets the server emits during a query. Open-ended in length, terminated by exactly one `EndOfStream` (success) or `Exception` (failure). See §5.4.

**Schema block** — synonym for "header block" (`NATIVE_FORMAT.md` §2.4). Used in the INSERT phase to denote the server's announcement of expected column shapes before the client sends data.

**Settings list** — a sequence of `(key, flags, value)` tuples in the Query body (§6.9), terminated by an empty key. Carries per-query application-layer configuration (§8.2).

**Stage** — a VarUInt field in the Query packet (§6.7 field 5) controlling how far the server executes the query: `0` = FetchColumns, `1` = WithMergeableState, `2` = Complete. External clients typically send `2`.

**Terminator** — a packet that ends a stream. The Query response ends on `EndOfStream` (success) or `Exception` (failure). The client's input stream ends on the empty Data marker.
