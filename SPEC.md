# ClickHouse Native Protocol Specification

**Status:** Work in progress. Covers protocol versions up to 54459.

**References:**
- ClickHouse server: `src/Core/ProtocolDefines.h`, `src/Server/TCPHandler.cpp`, `src/Client/Connection.cpp`
- ch-go client: `proto/` package
- ch-proto (this project): `src/proto/`

---

## 1. Overview

The ClickHouse native protocol is a binary, connection-oriented protocol over TCP. It is used for client-server communication and inter-server communication within ClickHouse clusters.

**Key properties:**
- **Transport:** TCP (optionally with TLS)
- **Byte order:** Little-endian for all fixed-width integers
- **Encoding:** Binary, positional (no field tags except in BlockInfo)
- **Versioning:** Protocol version negotiated during handshake; features are gated by version numbers
- **Connection model:** One query at a time per connection (no multiplexing)
- **Data format:** Columnar — data is sent as blocks of columns, not rows

Each message on the wire begins with a VarUInt packet type code, followed by the message body. The format of the body depends on the packet type and the negotiated protocol version.

---

## 2. Wire Format Primitives

All message fields are encoded using the following primitive types. There are no alignment or padding bytes between fields.

### 2.1 VarUInt

Variable-length unsigned integer using LEB-128 encoding. Each byte carries 7 data bits (bits 0-6) and 1 continuation bit (bit 7). The continuation bit is set (1) if more bytes follow, clear (0) on the final byte.

```
Value 0-127:        1 byte    (7 data bits)
Value 128-16383:    2 bytes   (14 data bits)
Value 16384-2097151: 3 bytes  (21 data bits)
...
Maximum:            10 bytes  (for full u64 range)
```

Encoding example for value `300`:
```
300 = 0b100101100
Byte 0: 0xAC = 0b10101100  (data: 0101100, continuation: 1)
Byte 1: 0x02 = 0b00000010  (data: 0000010, continuation: 0)
```

### 2.2 Fixed-Width Integers

| Type  | Size    | Encoding        |
|-------|---------|-----------------|
| UInt8 | 1 byte  | Raw byte        |
| UInt16| 2 bytes | Little-endian   |
| UInt32| 4 bytes | Little-endian   |
| UInt64| 8 bytes | Little-endian   |
| Int32 | 4 bytes | Little-endian, two's complement |
| Int64 | 8 bytes | Little-endian, two's complement |

### 2.3 String

Length-prefixed byte sequence:
```
[VarUInt: byte_length] [bytes: UTF-8 data]
```

An empty string is encoded as a single VarUInt `0` with no following bytes.

### 2.4 Bool

Single byte. `0x00` = false, any non-zero value = true (conventionally `0x01`).

---

## 3. Security

### 3.1 Transport Security (TLS)

TLS is handled at the transport layer, below the protocol. When TLS is enabled, the entire TCP stream is encrypted. The protocol messages themselves are identical whether TLS is used or not.

### 3.2 Authentication

Authentication occurs during the handshake via the ClientHello message. The `user` and `password` fields are sent as plaintext strings. Transport-level encryption (TLS) is expected to protect these credentials in transit.

SSH challenge-response authentication is available at protocol version 54466+ (not yet implemented in this spec).

### 3.3 Inter-Server Secret

For distributed query execution, servers authenticate to each other using a shared secret string sent in the Query message (see `cluster_secret` field). This is gated by `INTERSERVER_SECRET` (v54441).

---

## 4. Protocol Versioning & Feature Gates

### 4.1 Version Negotiation

Both client and server declare their maximum supported protocol version during the handshake. The **negotiated version** is the minimum of the two:

```
negotiated_version = min(client_version, server_version)
```

All subsequent messages on the connection use the negotiated version to determine which fields are present on the wire.

### 4.2 Feature Gates

A feature is identified by the protocol version in which it was introduced. A feature is **active** if the negotiated protocol version is >= the feature's version number.

When a feature is active, its associated fields **must** be present on the wire. The protocol is strictly positional — omitting a field corrupts the byte stream for all subsequent fields.

### 4.3 Feature Table

| Feature                         | Version | Affects              | Description |
|---------------------------------|---------|----------------------|-------------|
| BLOCK_INFO                      | 51903   | Block                | Block metadata (overflows, bucket number) |
| TIMEZONE                        | 54058   | ServerHello          | Server timezone string |
| QUOTA_KEY_IN_CLIENT_INFO        | 54060   | ClientInfo           | Quota key for resource limiting |
| DISPLAY_NAME                    | 54372   | ServerHello          | Server display name |
| VERSION_PATCH                   | 54401   | ServerHello, ClientInfo | Patch version number |
| WRITE_CLIENT_INFO               | 54420   | Query                | ClientInfo block in query |
| SETTINGS_SERIALIZED_AS_STRINGS  | 54429   | Query                | Settings as string key-value pairs |
| INTERSERVER_SECRET              | 54441   | Query                | Cluster authentication secret |
| OPEN_TELEMETRY                  | 54442   | ClientInfo           | OpenTelemetry trace context |
| DISTRIBUTED_DEPTH               | 54448   | ClientInfo           | Distributed query nesting depth |
| INITIAL_QUERY_START_TIME        | 54449   | ClientInfo           | Query start timestamp |
| PARALLEL_REPLICAS               | 54453   | ClientInfo           | Parallel replica coordination |
| ADDENDUM                        | 54458   | Handshake            | Post-handshake addendum (quota key) |
| PARAMETERS                      | 54459   | Query                | Parameterized query support |

---

## 5. Connection Lifecycle

### 5.1 Phases

```
  [Connect]
      |
      v
  HANDSHAKE --- error ---> [Disconnect]
      |
      ok
      |
      v
  READY <-------------------------------------+
      |                                        |
      |--- Ping -------> Pong -------------->--|
      |                                        |
      |--- Query ------> Streaming ---------->--|
      |                    |                   |
      |                    |--- Data blocks    |
      |                    |--- Progress       |
      |                    |--- Exception      |
      |                    |--- EndOfStream -->|
      |                                        |
      +----------------------------------------+
```

### 5.2 Handshake Phase

```
Client                              Server
  |                                    |
  |--- ClientHello ------------------->|
  |                                    |
  |<--- ServerHello -------------------|
  |     (or Exception)                 |
  |                                    |
  |    [version negotiation:           |
  |     min(client_ver, server_ver)]   |
  |                                    |
  |--- Addendum (if v >= 54458) ------>|
  |    (quota_key string)              |
  |                                    |
  [Connection Ready]
```

The client sends ClientHello first. The server responds with either ServerHello (success) or Exception (authentication failure, etc.).

After receiving ServerHello, the client computes the negotiated version. If the negotiated version supports the ADDENDUM feature (>= 54458), the client sends the addendum (currently just an empty quota_key string).

**Important:** The addendum is sent based on the **negotiated** version, not the client's version. Sending it when the server doesn't expect it will corrupt the connection.

### 5.3 Ping/Pong

A simple keepalive mechanism. Either side can send at any time when the connection is idle.

```
Client                              Server
  |--- Ping (packet type 4) --------->|
  |<--- Pong (packet type 4) ---------|
```

Both Ping and Pong are single VarUInt bytes with no payload.

### 5.4 Query Phase

```
Client                              Server
  |--- Query packet ------------------>|
  |--- ExternalTable (data block) ---->|  (optional, for temp tables)
  |--- Empty ExternalTable ----------->|  (end-of-data marker)
  |                                    |
  |<--- Data block --------------------|  (0 or more)
  |<--- Progress ----------------------|  (0 or more, interleaved)
  |<--- EndOfStream -------------------|  (query complete)
  |                                    |
  [Back to READY]
```

Or on error:
```
  |<--- Exception ---------------------|
  [Back to READY or Disconnect]
```

After sending the Query packet, the client must send at least one ExternalTable. An empty external table (empty table name + empty block) signals "no more client data." The server will not begin executing the query until it receives this end marker.

---

## 6. Message Reference

### Notation

Fields are listed in wire order. The `Type` column uses:
- `VarUInt` — variable-length unsigned integer (LEB-128)
- `String` — VarUInt-prefixed UTF-8 bytes
- `UInt8`, `Int32`, etc. — fixed-width little-endian integers
- `Bool` — single byte (0x00 or 0x01)
- `(conditional)` — field present only when the named feature is active

**Role** indicates who uses this field:
- **client** — set by external clients (clickhouse-client, JDBC, custom clients)
- **inter-server** — only meaningful for server-to-server communication in distributed queries; external clients write a default value (empty string, 0, false)
- **universal** — used by both external clients and inter-server communication

### 6.0 Packet Envelope

Every message on the wire follows the same outer structure:

```
[VarUInt: packet_type_code]    — always encoded as VarUInt
[message body]                  — format depends on packet_type_code
```

This applies to **both directions** (client → server and server → client). Complete packet type code tables are in section 9.

**Important:** The packet type is VarUInt, not a fixed-width byte. For values < 128 this produces the same single byte, but implementations must use VarUInt encoding to remain compatible if future packet types reach ≥128.

The following message tables document only the **body** of each packet (the bytes after the packet type code). Field numbering starts at `1` for the first body field.

### 6.1 ClientHello

**Direction:** Client -> Server
**Packet type:** `0`

Sent as the first message after TCP connection. No feature gating — all body fields are always present.

| # | Field             | Type    | Role      | Description |
|---|-------------------|---------|-----------|-------------|
| 1 | client_name       | String  | universal | Client identifier (e.g., "clickhouse-client") |
| 2 | version_major     | VarUInt | universal | Client major version |
| 3 | version_minor     | VarUInt | universal | Client minor version |
| 4 | protocol_version  | VarUInt | universal | Client's max supported protocol version |
| 5 | database          | String  | universal | Default database name |
| 6 | user              | String  | universal | Username for authentication |
| 7 | password          | String  | universal | Password (plaintext) |

### 6.2 ServerHello

**Direction:** Server -> Client
**Packet type:** `0`

Server's response to ClientHello on successful authentication.

| # | Field             | Type    | Role      | Condition               | Description |
|---|-------------------|---------|-----------|-------------------------|-------------|
| 1 | server_name       | String  | universal | always                  | Server identifier (e.g., "ClickHouse") |
| 2 | version_major     | VarUInt | universal | always                  | Server major version |
| 3 | version_minor     | VarUInt | universal | always                  | Server minor version |
| 4 | protocol_version  | VarUInt | universal | always                  | Server's protocol version |
| 5 | timezone          | String  | universal | TIMEZONE (v54058)       | Server timezone (e.g., "UTC") |
| 6 | display_name      | String  | universal | DISPLAY_NAME (v54372)   | Human-readable server name |
| 7 | version_patch     | VarUInt | universal | VERSION_PATCH (v54401)  | Server patch version |

### 6.3 Addendum

**Direction:** Client -> Server
**Condition:** Negotiated version >= 54458 (ADDENDUM)

Not a distinct packet type. Sent as raw fields immediately after the handshake completes.

| # | Field     | Type   | Role         | Description |
|---|-----------|--------|--------------|-------------|
| 1 | quota_key | String | inter-server | Resource quota identifier. Client sends empty string. |

### 6.4 Ping

**Direction:** Client -> Server
**Packet type:** `4`

No body. Just the packet envelope (single byte `0x04`).

### 6.5 Pong

**Direction:** Server -> Client
**Packet type:** `4`

No body. Just the packet envelope (single byte `0x04`).

### 6.6 Exception

**Direction:** Server -> Client
**Packet type:** `2`

Sent when the server encounters an error during any phase.

| # | Field       | Type    | Role      | Description |
|---|-------------|---------|-----------|-------------|
| 1 | code        | Int32   | universal | Error code |
| 2 | name        | String  | universal | Exception class (e.g., "DB::Exception") |
| 3 | message     | String  | universal | Human-readable error message |
| 4 | stack_trace | String  | universal | Server-side stack trace |
| 5 | has_nested  | Bool    | universal | If true, another Exception follows immediately |

If `has_nested` is true, the receiver should read another Exception structure immediately after (without a packet type prefix). This forms a chain of nested exceptions.

### 6.7 Query

**Direction:** Client -> Server
**Packet type:** `1`

| # | Field          | Type          | Role         | Condition                             | Description |
|---|----------------|---------------|--------------|---------------------------------------|-------------|
| 1 | query_id       | String        | universal    | always                                | Unique query identifier (UUID) |
| 2 | client_info    | ClientInfo    | universal    | WRITE_CLIENT_INFO (v54420)            | See section 6.8 |
| 3 | settings       | Setting[]     | universal    | SETTINGS_SERIALIZED_AS_STRINGS (v54429) | See section 6.9. Terminated by empty key. |
| 4 | cluster_secret | String        | inter-server | INTERSERVER_SECRET (v54441)           | Cluster auth. Client sends empty string. |
| 5 | stage          | VarUInt       | universal    | always                                | 0=FetchColumns, 1=WithMergeableState, 2=Complete |
| 6 | compression    | VarUInt       | universal    | always                                | 0=disabled, 1=enabled |
| 7 | query_body     | String        | universal    | always                                | SQL text |
| 8 | parameters     | Parameter[]   | client       | PARAMETERS (v54459)                   | See section 6.10. Terminated by empty key. |

### 6.8 ClientInfo

**Direction:** Client -> Server (embedded in Query)
**Condition:** WRITE_CLIENT_INFO (v54420)

Not a standalone packet. Encoded inline as part of the Query message.

| # | Field                      | Type    | Role         | Condition                          | Description |
|---|----------------------------|---------|--------------|------------------------------------|-----------------------------------------|
| 1 | query_kind                 | UInt8   | universal    | always                             | 0=NoQuery, 1=InitialQuery, 2=SecondaryQuery. Client sends `1`. `2` is inter-server only. |
| 2 | initial_user               | String  | universal    | always                             | User who initiated the query |
| 3 | initial_query_id           | String  | universal    | always                             | Original query ID |
| 4 | initial_address            | String  | universal    | always                             | Originating client socket address in `host:port` format. See §10.1. |
| 5 | initial_time               | Int64   | client       | INITIAL_QUERY_START_TIME (v54449)  | Query start time (microseconds). Fixed-width 8 bytes, not VarUInt. See §10.2. |
| 6 | query_interface            | UInt8   | universal    | always                             | 1=TCP, 2=HTTP |
| 7 | os_user                    | String  | client       | if interface=TCP                   | OS username |
| 8 | client_hostname            | String  | client       | if interface=TCP                   | Client machine hostname |
| 9 | client_name                | String  | client       | if interface=TCP                   | Client application name |
| 10| version_major              | VarUInt | universal    | if interface=TCP                   | Client major version |
| 11| version_minor              | VarUInt | universal    | if interface=TCP                   | Client minor version |
| 12| protocol_version           | VarUInt | universal    | if interface=TCP                   | Negotiated protocol version |
| 13| quota_key                  | String  | inter-server | QUOTA_KEY_IN_CLIENT_INFO (v54060)  | Resource quota key. Client sends empty string. |
| 14| distributed_depth          | VarUInt | inter-server | DISTRIBUTED_DEPTH (v54448)         | Distributed query nesting depth. Client sends `0`. |
| 15| version_patch              | VarUInt | universal    | VERSION_PATCH (v54401), TCP only   | Client patch version |
| 16| open_telemetry             | (below) | client       | OPEN_TELEMETRY (v54442)            | Trace context. Client sends `0` (no trace) if unused. |
| 17| collaborate_with_initiator | VarUInt | inter-server | PARALLEL_REPLICAS (v54453)         | Bool as VarUInt. Client sends `0`. |
| 18| count_participating_replicas | VarUInt | inter-server | PARALLEL_REPLICAS (v54453)       | Replica count. Client sends `0`. |
| 19| number_of_current_replica  | VarUInt | inter-server | PARALLEL_REPLICAS (v54453)         | Replica index. Client sends `0`. |

**OpenTelemetry encoding** (field 16):
```
[UInt8: has_trace]       — 0 = no trace data follows, 1 = trace data follows
If has_trace == 1:
  [16 bytes: trace_id]   — byte-swapped (see ClickHouse issue #34369)
  [8 bytes: span_id]     — byte-swapped
  [String: trace_state]  — W3C trace state
  [UInt8: trace_flags]   — W3C trace flags
```

### 6.9 Setting

Encoded inline as part of the Query message settings list. The list is terminated by a Setting with an empty key (just VarUInt `0`, no flags or value follow).

| # | Field | Type    | Role      | Description |
|---|-------|---------|-----------|-------------|
| 1 | key   | String  | universal | Setting name. Empty string = end of list. |
| 2 | flags | VarUInt | universal | Bit flags: 0x01=Important, 0x02=Custom, 0x04=Obsolete |
| 3 | value | String  | universal | Setting value as string |

Fields 2 and 3 are **not present** when key is empty (the terminator).

### 6.10 Parameter

Query parameters (for parameterized queries like `SELECT {x:UInt64}`). Encoded identically to a Setting with the `Custom` flag (0x02) set. Terminated by empty key, same as settings.

| # | Field | Type    | Role   | Description |
|---|-------|---------|--------|-------------|
| 1 | key   | String  | client | Parameter name. Empty = end of list. |
| 2 | flags | VarUInt | client | Always `0x02` (Custom) |
| 3 | value | String  | client | Parameter value as string |

### 6.11 Block (Data Packet)

A **Block** is the fundamental unit of data processing in ClickHouse — both internally within the query engine and on the wire. Understanding the Block is essential to understanding the protocol.

#### What is a Block?

A Block is a contiguous chunk of rows organized **columnar** — all values for column 1 are stored together, then all values for column 2, and so on. This is the same layout ClickHouse uses in memory and on disk:

```
Row-oriented (not used):           Columnar (used here):
┌──────┬──────┬──────┐             ┌──────────┐
│ row1 │ row1 │ row1 │             │ col1: v1 │
│ col1 │ col2 │ col3 │             │ col1: v2 │
├──────┼──────┼──────┤             │ col1: v3 │
│ row2 │ row2 │ row2 │             ├──────────┤
│ col1 │ col2 │ col3 │             │ col2: v1 │
├──────┼──────┼──────┤             │ col2: v2 │
│ row3 │ row3 │ row3 │             │ col2: v3 │
│ col1 │ col2 │ col3 │             └──────────┘
└──────┴──────┴──────┘             (and so on)
```

A Block contains only the **columns involved in the query**, not all columns of the underlying table. A `SELECT name, age FROM users` produces Blocks with 2 columns, not all columns of `users`.

#### Why columnar on the wire?

The columnar format is what gives the ClickHouse native protocol much of its performance advantage:

1. **No serialization conversion.** The server's in-memory representation is columnar. The client's consumption of analytical results is typically column-wise (aggregations, exports to Parquet/Arrow, etc.). Sending data columnar avoids any row-oriented intermediate format. No transpose step on either side.

2. **Better compression.** Values within a column are the same type and often have low cardinality or repetition (e.g., timestamps, enums, boolean flags). Column-by-column compression achieves much higher ratios than row-by-row compression of mixed types.

3. **Vectorized processing.** The client can process entire columns with SIMD or batched operations without splitting and regrouping by type.

4. **Streaming efficiency.** Large result sets are sent as multiple Blocks. Each Block is independent — the client can start processing column 1 of Block N while Block N+1 is still arriving on the socket.

Compare to row-oriented protocols (MySQL, PostgreSQL): every row must be encoded as a heterogeneous tuple, typed field-by-field. For analytical workloads returning millions of rows, this serialization cost dominates.

#### Block on the wire

**Direction:** Client -> Server or Server -> Client
**Packet type:** `2` (client → server, `ClientPacket::Data`) or `1` (server → client, `ServerPacket::Data`)

The wire format is **symmetric** — both directions include a `table_name` prefix:

**Client → Server** (used for INSERT data and external tables):
```
[VarUInt: 2]                     packet type (ClientPacket::Data)
[String: table_name]             external table name ("" for INSERT data / end marker)
[Block body]                     see below
```

**Server → Client** (used for result blocks):
```
[VarUInt: 1]                     packet type (ServerPacket::Data)
[String: table_name]             almost always "" for query results
[Block body]                     see below
```

The `table_name` is present in **both** directions. On server → client, it is effectively always empty. See §10.4.

#### table_name

| Field      | Type   | Role      | Condition               | Description |
|------------|--------|-----------|-------------------------|-------------|
| table_name | String | universal | TEMP_TABLES (v50264)    | External table name. Client: empty = end-of-data marker. Server: always empty for query results. |

#### BlockInfo

**Condition:** BLOCK_INFO (v51903)

Unlike all other protocol structures, BlockInfo uses **field-tagged encoding** for forward compatibility. Each field is preceded by a VarUInt field ID. A field ID of `0` terminates the structure. Fields may appear in any order, and unknown field IDs should be skipped.

| Field ID | Field         | Type  | Role         | Description |
|----------|---------------|-------|--------------|-------------|
| 1        | is_overflows  | UInt8 | inter-server | Overflow block from GROUP BY. Client sends `0` (false). |
| 2        | bucket_number | Int32 | inter-server | Aggregation bucket. Client sends `-1` (no bucket). |
| 0        | (terminator)  | —     | universal    | Marks end of BlockInfo. Always required. |

Wire encoding:
```
[VarUInt: 1] [UInt8: is_overflows]
[VarUInt: 2] [Int32: bucket_number]
[VarUInt: 0]
```

#### Block Body

| # | Field       | Type      | Role      | Description |
|---|-------------|-----------|-----------|-------------|
| 1 | block_info  | BlockInfo | universal | See above. Present if BLOCK_INFO (v51903) active. |
| 2 | num_columns | VarUInt   | universal | Number of columns |
| 3 | num_rows    | VarUInt   | universal | Number of rows |
| 4 | columns     | Column[]  | universal | One entry per column (see below). Not present if num_columns=0. |

#### Column

Repeated `num_columns` times:

| # | Field       | Type    | Role      | Description |
|---|-------------|---------|-----------|-------------|
| 1 | name        | String  | universal | Column name |
| 2 | type        | String  | universal | ClickHouse type name (e.g., "UInt64", "String") |
| 3 | data        | bytes   | universal | Type-specific binary encoding for all rows. See section 7. |

#### Empty Block

An empty block signals "end of data." It is used in two contexts:

1. **Client -> Server:** As the empty ExternalTable to mark end of client data after a Query packet.
2. **Server -> Client:** As the final block before EndOfStream (some server versions).

An empty block has `num_columns=0` and `num_rows=0` with no column entries. The full wire encoding of an empty Data packet (with BLOCK_INFO active), including the packet type and table_name, is:

```
[VarUInt: packet_type]              2 (client→server) or 1 (server→client)
[VarUInt: 0]                        table_name = "" (empty)

BlockInfo:
  [VarUInt: 1] [UInt8: 0x00]       is_overflows = false
  [VarUInt: 2] [Int32: FF FF FF FF] bucket_number = -1
  [VarUInt: 0]                      end of BlockInfo

Block body:
  [VarUInt: 0]                      num_columns = 0
  [VarUInt: 0]                      num_rows = 0
```

Total: 12 bytes. No column data follows.

---

## 7. Data Types & Column Encoding

> **Placeholder.** This section will document how each ClickHouse data type (UInt8, String, DateTime, Nullable, Array, etc.) is serialized within the column data portion of a Block.

---

## 8. Compression

> **Placeholder.** This section will document block-level compression. The compression frame format is:
> ```
> [16 bytes: CityHash128 checksum]
> [1 byte: method]         — 0x82=LZ4, 0x90=ZSTD, 0x02=None
> [4 bytes: compressed_size]
> [4 bytes: uncompressed_size]
> [N bytes: compressed_data]
> ```

---

## 9. Packet Type Reference

### Client -> Server

| Code | Name                      | Description |
|------|---------------------------|-------------|
| 0    | Hello                     | Handshake initiation |
| 1    | Query                     | Query execution request |
| 2    | Data                      | Data block (INSERT data, external tables) |
| 3    | Cancel                    | Cancel running query |
| 4    | Ping                      | Keepalive check |
| 5    | TablesStatusRequest       | Table status check |
| 6    | KeepAlive                 | Connection keepalive |
| 7    | Scalar                    | Scalar data block |
| 8    | IgnoredPartUUIDs          | Parts to exclude from query |
| 9    | ReadTaskResponse          | S3 cluster read response |
| 10   | MergeTreeReadTaskResponse | Parallel read task response |
| 11   | SSHChallengeRequest       | SSH auth challenge request |
| 12   | SSHChallengeResponse      | SSH auth challenge response |
| 13   | QueryPlan                 | Query plan |

### Server -> Client

| Code | Name                              | Description |
|------|-----------------------------------|-------------|
| 0    | Hello                             | Handshake response |
| 1    | Data                              | Result data block |
| 2    | Exception                         | Error |
| 3    | Progress                          | Query execution progress |
| 4    | Pong                              | Keepalive response |
| 5    | EndOfStream                       | Query complete |
| 6    | ProfileInfo                       | Profiling data |
| 7    | Totals                            | GROUP BY totals block |
| 8    | Extremes                          | Min/max values block |
| 9    | TablesStatusResponse              | Table status response |
| 10   | Log                               | Query execution logs |
| 11   | TableColumns                      | Column descriptions for defaults |
| 12   | PartUUIDs                         | Unique part IDs |
| 13   | ReadTaskRequest                   | Cluster read task request |
| 14   | ProfileEvents                     | Performance counters |
| 15   | MergeTreeAllRangesAnnouncement    | Parallel read initialization |
| 16   | MergeTreeReadTaskRequest          | Parallel read task assignment |
| 17   | TimezoneUpdate                    | Server timezone update |
| 18   | SSHChallenge                      | SSH auth challenge |

---

## 10. Implementation Notes

Discoveries and gotchas that aren't obvious from the wire format alone. Each entry documents the symptom, cause, and fix.

### 10.1 `ClientInfo.initial_address` must be non-empty in `host:port` format

**Symptom:** Server rejects Query with:
```
DB::Exception: Assertion violation: !hostAndPort.empty()
in file "base/poco/Net/src/SocketAddress.cpp", line 350
```

**Cause:** The server parses `initial_address` via Poco's `SocketAddress::init()`, which fails an assertion if the string is empty.

**Fix:** Always send a valid `host:port` string, e.g., `"127.0.0.1:0"`. Port `0` is fine — the server uses this only for logging, not for actual connections.

---

### 10.2 `ClientInfo.initial_time` is Int64, not VarUInt

**Symptom:** Server hangs or rejects Query with:
```
DB::Exception: Cannot read all data. Bytes read: 54. Bytes expected: 108.
(CANNOT_READ_ALL_DATA)
```

**Cause:** `initial_time` is written as a **fixed-width Int64 (8 bytes, little-endian)** on the server side (`writeBinary(initial_query_start_time_microseconds, out)` in `ClientInfo.cpp`). Encoding it as VarUInt underruns the server's expected byte count by 7 bytes (VarUInt encodes `0` in 1 byte vs. 8 bytes for Int64).

**Fix:** Use `write_i64` / `read_i64` for `initial_time`. Don't confuse this with other numeric fields in ClientInfo (`version_major`, `version_minor`, `protocol_version`, `distributed_depth`, etc.) which **are** VarUInt.

**General rule:** Within ClientInfo specifically, timestamps are fixed-width; everything else numeric is VarUInt. Check the C++ server's `ClientInfo.cpp` for the authoritative encoding of each field.

---

### 10.3 `BlockInfo.bucket_number` default is `-1`, not `0`

**Symptom:** Server misinterprets normal result blocks as belonging to aggregation bucket `0`, leading to incorrect distributed query behavior.

**Cause:** `0` is a valid bucket number (first bucket in a two-level GROUP BY aggregation). The "no bucket" sentinel is `-1`.

**Fix:** Default-construct `BlockInfo` with `bucket_number = -1`. Only set it to a non-negative value when actually emitting bucketed aggregation blocks (inter-server use only; external clients should always send `-1`).

---

### 10.4 Server → Client Data packet also carries `table_name`

**Symptom:** Client hangs on read after query, or decodes garbage for column names/types.

**Cause:** The Data packet wire format is **symmetric** — both directions include an empty `String` (table name) before the Block. The server's `sendData()` in `TCPHandler.cpp` writes `writeStringBinary("", *out)` after the packet type and before the Block payload. An earlier version of this spec incorrectly claimed only client→server had this prefix.

**Fix:** When reading a `ServerPacket::Data`, read a string first (the table name, almost always empty for query results) before calling `Block::decode`. See section 6.11 for the corrected wire diagram.

---

### 10.5 Packet type codes are VarUInt, not u8

**Symptom:** Works in testing (all current packet type codes are < 128, where VarUInt and u8 produce identical bytes), but future packet types ≥ 128 would silently break compatibility.

**Cause:** Every ClickHouse C++ send/receive path uses `writeVarUInt(Protocol::...)` / `readVarUInt(...)` for packet type codes, not fixed-byte writes.

**Fix:** Always use VarUInt encoding for packet type codes on both client and server sides. See section 6.0.

---
