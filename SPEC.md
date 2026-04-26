# ClickHouse Native Protocol & Native Format Specification

**Status:** Work in progress. Covers protocol versions up to 54459.

This document is the authoritative specification for two co-dependent pieces of the ClickHouse wire contract at the versions indicated above:

1. The **Native TCP protocol** — how packets are framed, how connections progress through handshake and query phases, and how non-Block message bodies are structured.
2. The **Native data format** — how tabular data (Blocks of columns) is serialized within Data-family packets.

The native TCP protocol always uses the Native data format on the wire; the two specifications are intentionally documented together.

Implementations should conform to this document.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Wire Format Primitives](#2-wire-format-primitives)
3. [Security](#3-security)
4. [Protocol Versioning & Feature Gates](#4-protocol-versioning--feature-gates)
5. [Packet Envelope](#5-packet-envelope)
6. [Connection Lifecycle](#6-connection-lifecycle) — **start here if implementing a client**
7. [Message Reference](#7-message-reference)
8. [Data Types & Column Encoding](#8-data-types--column-encoding) *(placeholder)*
9. [Compression](#9-compression) *(placeholder)*
10. [Packet Type Reference](#10-packet-type-reference)
11. [Implementation Notes](#11-implementation-notes)
12. [Configuration](#12-configuration)

---

## 1. Overview

The ClickHouse native protocol is a binary, connection-oriented protocol over TCP. It is used for client-server communication and inter-server communication within ClickHouse clusters.

**Key properties:**
- **Transport:** TCP (optionally with TLS)
- **Byte order:** Little-endian for all fixed-width integers
- **Encoding:** Binary, positional (no field tags except in BlockInfo, §7.11)
- **Versioning:** Protocol version negotiated during handshake; features are gated by version numbers
- **Connection model:** One query at a time per connection (no multiplexing)
- **Data format:** Columnar — data is sent as blocks of columns, not rows

Each message on the wire begins with a VarUInt packet type code, followed by the message body. The format of the body depends on the packet type and the negotiated protocol version.

### Scope: protocol and data format together

This document covers two closely-related but technically distinct specifications, bundled because they are co-dependent in the native TCP protocol:

1. **The native protocol** — packet framing, state machine, handshake, message structure. Everything in §5 (Packet Envelope) and §6 (Connection Lifecycle), plus the non-Block message bodies in §7 (Hello, Query, Exception, Progress, etc.).
2. **The Native data format** — how Blocks and their columns are serialized within Data/Totals/Extremes/Log/ProfileEvents packets. Covered by §7.11 (Block) and §8 (Data Types & Column Encoding).

### Native format is the only wire format over TCP

The native TCP protocol **always transmits tabular data in Native format**, regardless of any `FORMAT` clause in the SQL. This is important for implementers to understand:

- A query like `SELECT 1 FORMAT RowBinary` sent over the native TCP protocol still returns **Native-format** Block packets on the wire. The server's TCP handler constructs a `NativeWriter` for output unconditionally.
- The `FORMAT` clause is the client's responsibility to honor: `clickhouse-client` (the official CLI) receives Native blocks over TCP, then reformats them client-side into whatever the user requested (RowBinary, CSV, TabSeparated, JSON, Pretty, etc.).
- The HTTP interface is a separate code path where the server **does** honor the `FORMAT` clause and emits the requested format on the wire. That interface is out of scope for this spec.
- For INSERT queries, the `FORMAT` clause **is** honored by the TCP server — but only for parsing client-provided input data (`INSERT INTO t FORMAT CSV`). Client→server data on the inbound side can be in non-Native formats; server→client data on the outbound side is always Native.

A correct native-TCP client therefore needs to:
- Implement Native-format decoding (§7.11 and §8).
- Optionally, for compatibility with the `FORMAT` clause, reformat received blocks into other output formats — but that is a client-side concern, not a protocol concern.

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

Authentication occurs during the handshake via the ClientHello message (§7.1). The `user` and `password` fields are sent as plaintext strings. Transport-level encryption (TLS) is expected to protect these credentials in transit.

SSH challenge-response authentication is available at protocol version 54466+ (not yet covered by this spec).

### 3.3 Inter-Server Secret

For distributed query execution, servers authenticate to each other using a shared secret string sent in the Query message (see `cluster_secret` field in §7.7). This is gated by `INTERSERVER_SECRET` (v54441). External clients should always send an empty string.

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
| SERVER_LOGS                     | 54406   | Log                  | Server sends Log packets (when `send_logs_level` is set) |
| WRITE_CLIENT_INFO               | 54420   | Query, Progress      | ClientInfo block in Query; `wrote_rows`/`wrote_bytes` in Progress |
| SETTINGS_SERIALIZED_AS_STRINGS  | 54429   | Query                | Settings as string key-value pairs |
| INTERSERVER_SECRET              | 54441   | Query                | Cluster authentication secret |
| OPEN_TELEMETRY                  | 54442   | ClientInfo           | OpenTelemetry trace context |
| DISTRIBUTED_DEPTH               | 54448   | ClientInfo           | Distributed query nesting depth |
| INITIAL_QUERY_START_TIME        | 54449   | ClientInfo           | Query start timestamp |
| PROFILE_EVENTS                  | 54451   | ProfileEvents        | Server sends ProfileEvents packets |
| PARALLEL_REPLICAS               | 54453   | ClientInfo           | Parallel replica coordination |
| CUSTOM_SERIALIZATION            | 54454   | Block (Column)       | Per-column serialization flag; enables sparse/dictionary/etc. on-wire formats |
| ADDENDUM                        | 54458   | Handshake            | Post-handshake addendum (quota key) |
| PARAMETERS                      | 54459   | Query                | Parameterized query support |
| SERVER_QUERY_TIME_IN_PROGRESS   | 54460   | Progress             | `elapsed_ns` field in Progress |
| ROWS_BEFORE_AGGREGATION         | 54469   | ProfileInfo          | `applied_aggregation` and `rows_before_aggregation` fields in ProfileInfo |

---

## 5. Packet Envelope

Every message on the wire follows the same outer structure:

```
[VarUInt: packet_type_code]    — always encoded as VarUInt
[message body]                  — format depends on packet_type_code
```

This applies to **both directions** (client → server and server → client). Complete packet type code tables are in §10.

**Important:** The packet type is VarUInt, not a fixed-width byte. For values < 128 this produces the same single byte, but implementations must use VarUInt encoding to remain compatible if future packet types reach ≥128. See §11.5.

Message tables in §7 document only the **body** of each packet (the bytes after the packet type code). Field numbering starts at `1` for the first body field.

---

## 6. Connection Lifecycle

This section is the **primary reference for client implementers**. Each state machine phase describes what to send, what to expect, and how to transition — with cross-references to the message formats in §7.

### Why this is a state machine

The native protocol is **strictly stateful**: at every point in time the connection has exactly one role — handshaking, idle (waiting for the next command), or processing a query. The protocol does not tag packets with stream IDs or request IDs, and it does not support concurrent operations on one connection. A client that sends a new request before the previous response has fully drained will interleave bytes on the wire and produce an unparseable stream on the server's side.

The state machine in this section defines precisely when each type of packet may be sent or received. Clients must conform to this model; deviations that "happen to work" on one code path will break in adversarial ordering (e.g., an exception mid-query, a cancellation request, or a concurrent Ping).

### 6.1 States

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

A connection is always in exactly one of: `HANDSHAKE`, `READY`, `READING_RESPONSE`, or terminated.

**State descriptions:**

- **`HANDSHAKE`** — the initial state after the TCP connection is established. Only the handshake messages (§6.2) are valid in this state. The connection transitions to `READY` on success, or terminates on failure (bad credentials, protocol mismatch, etc.).

- **`READY`** — the idle state. The server is waiting for the client to initiate the next operation. The only valid actions from `READY` are: send a Ping (→ §6.3), send a Query (→ §6.4), or close the connection. Between commands the connection may remain in `READY` indefinitely (subject to the server's `idle_connection_timeout` — see §12.1.3).

- **`READING_RESPONSE`** — entered when the client sends a Query. The client must fully drain the server's response stream (§6.4) before the connection returns to `READY`. While in this state, the client must not send any other request. The only client → server packet allowed is a Cancel (not yet specified in this document).

- **Terminated** — the connection is no longer usable. Terminated connections cannot be re-used; the client must establish a new TCP connection and restart the handshake.

### 6.2 Handshake Phase

The handshake is the **authentication and negotiation** phase. Its job is to (a) verify the client's credentials, (b) agree on a protocol version that both sides support, and (c) exchange identifying information (server version, timezone, etc.). The handshake happens exactly once per connection and is non-resumable — a connection that fails handshake cannot be reused.

**Why it matters:** every subsequent message on the connection depends on the **negotiated protocol version** computed during this phase. A client that skips handshake, gets the version wrong, or sends packets in the wrong order will corrupt every subsequent packet on the connection. See §11.15 for the silent-failure mode when the client declares too low a protocol version.

#### Precondition
TCP connection just established. No messages exchanged yet.

#### Flow
```
Client                              Server
  |--- ClientHello ------------------>|
  |<--- ServerHello ------------------|    or Exception
  |                                   |
  |    (compute negotiated_version)   |
  |                                   |
  |--- Addendum ---------------------->|    only if negotiated_version ≥ 54458
```

#### Step 1: Client sends ClientHello

See §7.1 for the wire format.

The client sends its maximum supported protocol version in `protocol_version`. The server will respond with its own version, and both sides will then use `min(client_version, server_version)` as the negotiated version for all subsequent messages.

#### Step 2: Client reads response

Dispatch on the packet type (§5):

| Packet type (§10.2)       | Action |
|---------------------------|--------|
| `Hello` (0) → §7.2        | Decode ServerHello. Compute `negotiated_version = min(client_ver, server_ver)`. Proceed to Step 3. |
| `Exception` (2) → §7.6    | Decode Exception. Return as error. Terminate connection. |
| anything else             | Protocol violation. Terminate connection. |

#### Step 3: Addendum (conditional)

If `negotiated_version ≥ 54458` (feature `ADDENDUM`, §4.3), the client must send an Addendum. See §7.3.

**Critical:** The decision to send the addendum is based on the **negotiated** version, not the client's declared version. Sending it when the server doesn't expect it will misalign subsequent messages and corrupt the connection.

#### Postcondition
On success, connection transitions to `READY`. On any error, terminate the connection.

---

### 6.3 Ping Phase

The Ping phase is an **application-level keepalive / liveness check**. Unlike TCP keepalive (which is a kernel-level probe, see §12.1.1), a successful Ping/Pong round-trip confirms that:

1. The TCP connection is alive in both directions.
2. The server process is responsive and scheduling work (not deadlocked).
3. The server's native-protocol handler is correctly parsing packets.

Clients typically use Ping to probe a connection's health before issuing a query, especially after an idle period — a Ping failure is much faster to act on than a failing Query. A connection that has been sitting idle for minutes may have been closed by an intermediary (NAT, firewall, LB) without either side's kernel being notified; Ping surfaces this quickly.

Ping is stateless — the server does not track that a Ping was sent. The Ping/Pong pair is not correlated with any query and does not affect query state. Multiple sequential Pings on the same connection are valid and independent.

#### Precondition
Connection in `READY` state.

#### Flow
```
Client                              Server
  |--- Ping (0x04) ------------------->|
  |<--- Pong (0x04) -------------------|
```

#### Step 1: Client sends Ping
See §7.4.

#### Step 2: Client reads response

| Packet type (§10.2)       | Action |
|---------------------------|--------|
| `Pong` (4) → §7.5         | Keepalive confirmed. Transition back to `READY`. |
| `Exception` (2) → §7.6    | Decode, return as error. |
| anything else             | Protocol violation. |

---

### 6.4 Query Phase

The Query phase is where the real work happens — the client submits a SQL statement along with associated data (external tables, INSERT data), and the server streams back a sequence of result blocks and execution telemetry.

This is the most complex phase for two reasons:

1. **Asymmetric send/receive.** The client sends a bounded sequence of packets (Query + optional external tables + a terminating empty Data packet), then transitions to a receive-only mode. It cannot issue another command or cancel cleanly except through a specific Cancel packet.

2. **Server response is a stream, not a single reply.** The response is not "one packet of results"; it is an open-ended sequence of Data, Progress, Log, ProfileEvents, and other packets, terminated by exactly one `EndOfStream` or `Exception`. The client must loop until it sees the terminator.

**Key concepts for this phase:**

- **Query packet:** carries the SQL text plus metadata (query_id, ClientInfo, settings, parameters, stage flag, compression flag). See §7.7.
- **External tables:** zero or more temporary tables attached to this query, referenced from the SQL. See §6.4 Step 2 and §7.11.
- **End-of-client-data marker:** an empty Data packet (0 columns, 0 rows). The server does **not** begin executing the query until it receives this marker — even for queries with no associated data.
- **Response stream:** the sequence of packets the server emits during execution. See §11.6 for the critical "first Data packet is the schema header (0 rows)" behavior.
- **Termination:** only `EndOfStream` (on success) or `Exception` (on failure) ends the response stream. `num_rows == 0` is **not** a terminator (see §11.6).

**Typical error modes:**

- Client forgets the end-of-client-data marker → server hangs waiting for more input; client hangs waiting for response. See §11.x-ish debugging pattern.
- Client stops reading at the first Data packet (the header block) → sees 0 rows even for queries that return results. See §11.6.
- Client attempts to send a new Query while still in `READING_RESPONSE` → stream corruption.

#### Precondition
Connection in `READY` state.

#### Flow
```
Client                              Server
  |--- Query packet ------------------>|     §7.7
  |--- ExternalTable (data) ---------->|     §7.11  (optional, for temp tables)
  |--- Empty Data marker ------------->|     §7.11  (required, end-of-client-data)
  |                                    |
  |<--- Data (header block) -----------|     §7.11  schema: N cols, 0 rows
  |<--- Progress ----------------------|     0 or more, interleaved
  |<--- Log ---------------------------|     0 or more (if server logs enabled)
  |<--- Data (result block) -----------|     0 or more: N cols, M rows each
  |<--- Totals / Extremes -------------|     0 or more (aggregation queries)
  |<--- ProfileInfo / ProfileEvents ---|     0 or more (profiling)
  |<--- Data (empty block) ------------|     boundary marker (NOT the end)
  |<--- Progress ----------------------|     final updates
  |<--- EndOfStream ------------------>|     authoritative end of query
```

On error at any point:
```
  |<--- Exception --------------------->|    terminates the query
```

#### Step 1: Client sends Query packet

See §7.7. The client must choose a unique `query_id` (typically a UUID).

#### Step 2: Client sends end-of-client-data marker

The server will not begin executing the query until it receives at least one Data packet after the Query. For a plain `SELECT`, this is the empty Data packet (see "Empty Data packet" below).

For `INSERT` queries or queries using external tables, the client first sends Data packets containing the actual data, then the empty Data packet to signal "no more data."

See §7.11 for the Data packet format. The empty Data packet has:
- `table_name = ""`
- `num_columns = 0`
- `num_rows = 0`

#### Step 3: Client transitions to `READING_RESPONSE` and flushes

The client must flush its write buffer at this point. Without a flush, the server may block waiting for input while the client blocks waiting for output.

#### Step 4: Client reads response packets in a loop

The server sends a stream of packets. The client loops, reading one packet envelope (§5) at a time, and dispatches by packet type:

| Packet type (§10.2)          | Action |
|------------------------------|--------|
| `Data` (1) → §7.11           | Decode the block. First Data = schema header (§7.11). Subsequent = result blocks (accumulate). Empty block (0/0) = boundary marker. **Do not treat num_rows=0 as end-of-query.** |
| `Progress` (3)               | Execution metrics (rows read, bytes read, elapsed time). May be aggregated or ignored. |
| `EndOfStream` (5)            | Query complete. **Exit the loop.** Transition to `READY`. |
| `ProfileInfo` (6)            | Profiling data. May be ignored. |
| `Totals` (7)                 | Aggregation totals block (same wire format as Data). |
| `Extremes` (8)               | Min/max values block (same wire format as Data). |
| `Log` (10)                   | Server log line. May be ignored. |
| `TableColumns` (11)          | Column defaults metadata. May be ignored. |
| `ProfileEvents` (14)         | Performance counters. May be ignored. |
| `Exception` (2) → §7.6       | Decode and return as error. Exit the loop. Transition to `READY`. |
| anything else                | Unexpected during query phase. Terminate connection. |

See §11.6 for the common "SELECT returns 0 rows" pitfall.

#### Postcondition
On `EndOfStream` or handled `Exception`, connection returns to `READY`. On protocol violation or I/O error, terminate the connection.

---

## 7. Message Reference

### Notation

Fields are listed in wire order. The `Type` column uses:
- `VarUInt` — variable-length unsigned integer (LEB-128, §2.1)
- `String` — VarUInt-prefixed UTF-8 bytes (§2.3)
- `UInt8`, `Int32`, etc. — fixed-width little-endian integers (§2.2)
- `Bool` — single byte, 0x00 or 0x01 (§2.4)

**Role** indicates who uses this field:
- **client** — set by external clients (clickhouse-client, JDBC, custom clients)
- **inter-server** — only meaningful for server-to-server communication in distributed queries; external clients write a default value (empty string, 0, false)
- **universal** — used by both external clients and inter-server communication

Message tables document only the **body** of each packet (after the packet type byte of §5). Field numbering starts at `1` for the first body field.

### 7.1 ClientHello

**Direction:** Client → Server
**Packet type:** `0`
**Referenced by:** §6.2 (handshake phase)

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

### 7.2 ServerHello

**Direction:** Server → Client
**Packet type:** `0`
**Referenced by:** §6.2 (handshake phase)

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

### 7.3 Addendum

**Direction:** Client → Server
**Condition:** Negotiated version ≥ 54458 (ADDENDUM)
**Referenced by:** §6.2 (handshake phase)

Not a distinct packet type. Sent as raw fields immediately after the handshake completes. There is **no** packet type byte prefix.

| # | Field     | Type   | Role         | Description |
|---|-----------|--------|--------------|-------------|
| 1 | quota_key | String | inter-server | Resource quota identifier. External clients send empty string. |

### 7.4 Ping

**Direction:** Client → Server
**Packet type:** `4`
**Referenced by:** §6.3 (ping phase)

No body. Just the packet envelope — a single byte `0x04` on the wire.

### 7.5 Pong

**Direction:** Server → Client
**Packet type:** `4`
**Referenced by:** §6.3 (ping phase)

No body. Just the packet envelope — a single byte `0x04` on the wire.

### 7.6 Exception

**Direction:** Server → Client
**Packet type:** `2`
**Referenced by:** §6.2, §6.3, §6.4

Sent when the server encounters an error during any phase.

| # | Field       | Type    | Role      | Description |
|---|-------------|---------|-----------|-------------|
| 1 | code        | Int32   | universal | Error code |
| 2 | name        | String  | universal | Exception class (e.g., "DB::Exception") |
| 3 | message     | String  | universal | Human-readable error message |
| 4 | stack_trace | String  | universal | Server-side stack trace |
| 5 | has_nested  | Bool    | universal | If true, another Exception follows immediately |

If `has_nested` is true, the receiver should read another Exception structure immediately after (without a packet type prefix). This forms a chain of nested exceptions.

### 7.7 Query

**Direction:** Client → Server
**Packet type:** `1`
**Referenced by:** §6.4 (query phase)

| # | Field          | Type          | Role         | Condition                             | Description |
|---|----------------|---------------|--------------|---------------------------------------|-------------|
| 1 | query_id       | String        | universal    | always                                | Unique query identifier (UUID) |
| 2 | client_info    | ClientInfo    | universal    | WRITE_CLIENT_INFO (v54420)            | See §7.8 |
| 3 | settings       | Setting[]     | universal    | SETTINGS_SERIALIZED_AS_STRINGS (v54429) | See §7.9. Terminated by empty key. |
| 4 | cluster_secret | String        | inter-server | INTERSERVER_SECRET (v54441)           | Cluster auth. External clients send empty string. |
| 5 | stage          | VarUInt       | universal    | always                                | 0=FetchColumns, 1=WithMergeableState, 2=Complete |
| 6 | compression    | VarUInt       | universal    | always                                | 0=disabled, 1=enabled |
| 7 | query_body     | String        | universal    | always                                | SQL text |
| 8 | parameters     | Parameter[]   | client       | PARAMETERS (v54459)                   | See §7.10. Terminated by empty key. |

### 7.8 ClientInfo

**Direction:** Client → Server (embedded in Query)
**Condition:** WRITE_CLIENT_INFO (v54420)
**Referenced by:** §7.7

Not a standalone packet. Encoded inline as part of the Query message body (field 2).

| # | Field                        | Type    | Role         | Condition                          | Description |
|---|------------------------------|---------|--------------|------------------------------------|-----------------------------------------|
| 1 | query_kind                   | UInt8   | universal    | always                             | 0=NoQuery, 1=InitialQuery, 2=SecondaryQuery. External clients send `1`. `2` is inter-server only. |
| 2 | initial_user                 | String  | universal    | always                             | User who initiated the query |
| 3 | initial_query_id             | String  | universal    | always                             | Original query ID |
| 4 | initial_address              | String  | universal    | always                             | Originating client socket address in `host:port` format. See §11.1. |
| 5 | initial_time                 | Int64   | client       | INITIAL_QUERY_START_TIME (v54449)  | Query start time (microseconds). **Fixed-width 8 bytes**, not VarUInt. See §11.2. |
| 6 | query_interface              | UInt8   | universal    | always                             | 1=TCP, 2=HTTP |
| 7 | os_user                      | String  | client       | if interface=TCP                   | OS username |
| 8 | client_hostname              | String  | client       | if interface=TCP                   | Client machine hostname |
| 9 | client_name                  | String  | client       | if interface=TCP                   | Client application name |
| 10| version_major                | VarUInt | universal    | if interface=TCP                   | Client major version |
| 11| version_minor                | VarUInt | universal    | if interface=TCP                   | Client minor version |
| 12| protocol_version             | VarUInt | universal    | if interface=TCP                   | Negotiated protocol version |
| 13| quota_key                    | String  | inter-server | QUOTA_KEY_IN_CLIENT_INFO (v54060)  | Resource quota key. External clients send empty string. |
| 14| distributed_depth            | VarUInt | inter-server | DISTRIBUTED_DEPTH (v54448)         | Distributed query nesting depth. External clients send `0`. |
| 15| version_patch                | VarUInt | universal    | VERSION_PATCH (v54401), TCP only   | Client patch version |
| 16| open_telemetry               | (below) | client       | OPEN_TELEMETRY (v54442)            | Trace context. Clients without tracing send `0` (no trace). |
| 17| collaborate_with_initiator   | VarUInt | inter-server | PARALLEL_REPLICAS (v54453)         | Bool as VarUInt. External clients send `0`. |
| 18| count_participating_replicas | VarUInt | inter-server | PARALLEL_REPLICAS (v54453)         | Replica count. External clients send `0`. |
| 19| number_of_current_replica    | VarUInt | inter-server | PARALLEL_REPLICAS (v54453)         | Replica index. External clients send `0`. |

**OpenTelemetry encoding** (field 16):
```
[UInt8: has_trace]              0 = no trace data follows, 1 = trace data follows
If has_trace == 1:
  [16 bytes: trace_id]          byte-swapped per-8-bytes (historical quirk)
  [8 bytes: span_id]            byte-swapped
  [String: trace_state]         W3C trace state
  [UInt8: trace_flags]          W3C trace flags
```

### 7.9 Setting

Encoded inline as part of the Query message settings list (§7.7, field 3). The list is terminated by a Setting with an empty key (just VarUInt `0`, no flags or value follow).

| # | Field | Type    | Role      | Description |
|---|-------|---------|-----------|-------------|
| 1 | key   | String  | universal | Setting name. Empty string = end of list. |
| 2 | flags | VarUInt | universal | Bit flags: 0x01=Important, 0x02=Custom, 0x04=Obsolete |
| 3 | value | String  | universal | Setting value as string |

Fields 2 and 3 are **not present** when key is empty (the terminator).

### 7.10 Parameter

Query parameters (for parameterized queries like `SELECT {x:UInt64}`). Encoded identically to a Setting with the `Custom` flag (0x02) set. Terminated by empty key, same as settings.

| # | Field | Type    | Role   | Description |
|---|-------|---------|--------|-------------|
| 1 | key   | String  | client | Parameter name. Empty = end of list. |
| 2 | flags | VarUInt | client | Always `0x02` (Custom) |
| 3 | value | String  | client | Parameter value as string |

### 7.11 Block (Data Packet)

A **Block** is the fundamental unit of data processing in ClickHouse — both internally within the query engine and on the wire. Understanding the Block is essential to understanding the protocol.

**Direction:** Client → Server or Server → Client
**Packet type:** `2` (client → server) or `1` (server → client)
**Referenced by:** §6.4 (query phase)

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
2. **Better compression.** Values within a column are the same type and often have low cardinality or repetition. Column-by-column compression achieves much higher ratios than row-by-row compression of mixed types.
3. **Vectorized processing.** The client can process entire columns with SIMD or batched operations without splitting and regrouping by type.
4. **Streaming efficiency.** Large result sets are sent as multiple Blocks. Each Block is independent — the client can start processing column 1 of Block N while Block N+1 is still arriving on the socket.

Compare to row-oriented protocols (MySQL, PostgreSQL): every row must be encoded as a heterogeneous tuple, typed field-by-field. For analytical workloads returning millions of rows, this serialization cost dominates.

#### Data packet wire format

The wire format is **symmetric** — both directions include a `table_name` prefix. The only difference is the packet type byte (§5): `2` for client → server, `1` for server → client.

```
[VarUInt: packet_type]     2 (client→server) or 1 (server→client)
[String: table_name]       External table name; "" in most cases
[BlockInfo]                Metadata (if BLOCK_INFO feature active, §4.3)
[VarUInt: num_columns]     Number of columns in this block
[VarUInt: num_rows]        Number of rows in this block
[Column × num_columns]     Column entries, omitted if num_columns = 0
```

#### table_name field

| Field      | Type   | Role      | Condition               | Description |
|------------|--------|-----------|-------------------------|-------------|
| table_name | String | universal | TEMP_TABLES (v50264)    | External table name. Client: empty = end-of-data marker. Server: always empty for query results. See §11.4. |

#### BlockInfo

**Condition:** BLOCK_INFO (v51903)

Unlike all other protocol structures, BlockInfo uses **field-tagged encoding** for forward compatibility. Each field is preceded by a VarUInt field ID. A field ID of `0` terminates the structure. Fields may appear in any order, and unknown field IDs should be skipped.

| Field ID | Field         | Type  | Role         | Description |
|----------|---------------|-------|--------------|-------------|
| 1        | is_overflows  | UInt8 | inter-server | Overflow block from GROUP BY. External clients send `0` (false). |
| 2        | bucket_number | Int32 | inter-server | Aggregation bucket. External clients send `-1` (no bucket). See §11.3. |
| 0        | (terminator)  | —     | universal    | Marks end of BlockInfo. Always required. |

Wire encoding:
```
[VarUInt: 1] [UInt8: is_overflows]
[VarUInt: 2] [Int32: bucket_number]
[VarUInt: 0]
```

#### Column

Repeated `num_columns` times. Only present when `num_columns > 0`.

| # | Field                | Type    | Role      | Condition                     | Description |
|---|----------------------|---------|-----------|-------------------------------|-------------|
| 1 | name                 | String  | universal | always                        | Column name |
| 2 | type                 | String  | universal | always                        | ClickHouse type name (e.g., "UInt64", "String") |
| 3 | has_custom_serialization | UInt8 | universal | CUSTOM_SERIALIZATION (v54454) | 0 = default serialization, 1 = custom (kind_stack follows). See §11.7. |
| 4 | kind_stack           | bytes   | universal | if field 3 == 1               | Opaque serialization metadata describing the non-default format (sparse, low-cardinality, etc.). Not yet specified in this document. |
| 5 | data                 | bytes   | universal | always                        | Column values for all `num_rows` rows. Layout depends on the type and (if custom) the kind_stack. See §8. |

**Custom serialization** (field 3 = 1) enables the server to transmit columns in non-default on-wire formats:
- **Sparse** — columns with mostly default values transmit only non-defaults + indices
- **Low-cardinality / dictionary** — columns with few unique values transmit a dictionary + indices
- Other variants may exist

For most use cases, clients write `0` (default serialization) and reject non-zero values from the server if they don't implement custom decoding. Columns in INSERT data from external clients are almost always default serialization.

**Omitting this field when the feature is active** is a common bug — see §11.7.

#### Block variants and their meaning

All Data packets use the same wire format above. Their **meaning** depends on their content and position in the response stream:

| Variant            | `num_columns` | `num_rows` | Position in stream      | Purpose |
|--------------------|---------------|------------|-------------------------|---------|
| **Header block**   | N > 0         | **0**      | 1st Data after a Query  | Announces the result schema (column names + types). Sent exactly once per query. |
| **Result block**   | N > 0         | M > 0      | 0 or more after header  | Actual result data. Same column order as the header. Large result sets stream as multiple result blocks. |
| **Empty block**    | 0             | 0          | Client: end-of-data marker before server responds. Server: boundary marker near end of response stream (not the query terminator). | |

Structurally, these three are just different fillings of the same wire format. A client that loops on `ServerPacket::Data` naturally handles all three; the key rule is **never treat `num_rows = 0` as end-of-query**. Only `EndOfStream` (packet type 5, §10.2) signals the end of a query. See §11.6.

#### Byte-level examples

##### Example 1: Empty Data packet (12 bytes, with BLOCK_INFO active)

Used as the client's end-of-client-data marker (§6.4 Step 2), and also sent by the server as a boundary marker in the response stream.

```
02                      packet_type = 2 (client→server) or 01 for server→client
00                      table_name = "" (varuint length 0, no bytes follow)
01 00                   BlockInfo field_id=1, is_overflows = 0 (false)
02 FF FF FF FF          BlockInfo field_id=2, bucket_number = -1 (Int32 LE)
00                      BlockInfo terminator (field_id=0)
00                      num_columns = 0
00                      num_rows = 0
```

##### Example 2: Header block for `SELECT 1`

The server announces one column named `"1"` of type `UInt8`, with zero rows. At protocol ≥ 54454, the `has_custom_serialization` byte is included.

```
01                      packet_type = 1 (ServerPacket::Data)
00                      table_name = ""
01 00                   BlockInfo: is_overflows = 0
02 FF FF FF FF          BlockInfo: bucket_number = -1
00                      BlockInfo terminator
01                      num_columns = 1
00                      num_rows = 0
01 "1"                  Column[0].name = "1"
05 "UInt8"              Column[0].type = "UInt8"
00                      Column[0].has_custom_serialization = 0 (v54454+)
                        Column[0].data: no bytes (num_rows is 0)
```

##### Example 3: Result block for `SELECT 1` (the row)

Same structure as the header but with `num_rows = 1` and 1 byte of column data.

```
01                      packet_type = 1
00                      table_name = ""
01 00                   BlockInfo: is_overflows = 0
02 FF FF FF FF          BlockInfo: bucket_number = -1
00                      BlockInfo terminator
01                      num_columns = 1
01                      num_rows = 1
01 "1"                  Column[0].name = "1"
05 "UInt8"              Column[0].type = "UInt8"
00                      Column[0].has_custom_serialization = 0 (v54454+)
01                      Column[0].data: one UInt8 byte = 1
```

### 7.12 Progress

**Direction:** Server → Client
**Packet type:** `3`
**Referenced by:** §6.4 (query phase)

Progress packets are emitted by the server periodically during query execution to report throughput. A client may aggregate these for a progress bar, logging, or metrics — or ignore them entirely. The packet is sent between Data packets in the response stream and does **not** signal end-of-stream.

All fields are VarUInt.

| # | Field        | Type    | Role      | Condition                                 | Description |
|---|--------------|---------|-----------|-------------------------------------------|-------------|
| 1 | rows         | VarUInt | universal | always                                    | Rows processed so far |
| 2 | bytes        | VarUInt | universal | always                                    | Bytes processed so far |
| 3 | total_rows   | VarUInt | universal | always                                    | Estimated total rows to process (may be 0 if unknown) |
| 4 | wrote_rows   | VarUInt | universal | WRITE_CLIENT_INFO (v54420)                | Rows written (for INSERT queries) |
| 5 | wrote_bytes  | VarUInt | universal | WRITE_CLIENT_INFO (v54420)                | Bytes written (for INSERT queries) |
| 6 | elapsed_ns   | VarUInt | universal | SERVER_QUERY_TIME_IN_PROGRESS (v54460)    | Elapsed nanoseconds since query start |

Multiple Progress packets may be sent during one query; each contains **cumulative** totals, not deltas.

### 7.13 ProfileInfo

**Direction:** Server → Client
**Packet type:** `6`
**Referenced by:** §6.4 (query phase)

Sent near the end of query execution to report post-execution statistics, particularly around LIMIT clauses. Unlike Progress (cumulative during execution), ProfileInfo is sent **once** per query.

| # | Field                          | Type    | Role      | Condition                            | Description |
|---|--------------------------------|---------|-----------|--------------------------------------|-------------|
| 1 | rows                           | VarUInt | universal | always                               | Total rows processed |
| 2 | blocks                         | VarUInt | universal | always                               | Total blocks processed |
| 3 | bytes                          | VarUInt | universal | always                               | Total bytes processed |
| 4 | applied_limit                  | Bool    | universal | always                               | Whether a LIMIT clause was applied |
| 5 | rows_before_limit              | VarUInt | universal | always                               | Row count before LIMIT was applied (useful for "Showing X of Y" displays) |
| 6 | calculated_rows_before_limit   | Bool    | universal | always                               | Whether `rows_before_limit` is meaningful. If false, the server did not compute it. |
| 7 | applied_aggregation            | Bool    | universal | ROWS_BEFORE_AGGREGATION (v54469)     | Whether aggregation (GROUP BY) was applied |
| 8 | rows_before_aggregation        | VarUInt | universal | ROWS_BEFORE_AGGREGATION (v54469)     | Row count before aggregation |

### 7.14 Totals

**Direction:** Server → Client
**Packet type:** `7`
**Referenced by:** §6.4 (query phase)

Carries the "totals" row produced by queries with `WITH TOTALS` (e.g., `SELECT sum(x) FROM t GROUP BY y WITH TOTALS`). The totals row aggregates over all groups.

**Wire format: identical to Data (§7.11)** — a `table_name` string (always empty) followed by a Block. Only the packet type byte differs.

```
[VarUInt: 7]                     packet type (ServerPacket::Totals)
[String: table_name]             always ""
[Block body]                     same format as §7.11
```

Not emitted by queries that don't use `WITH TOTALS`. Clients that don't implement `WITH TOTALS` support should still decode this packet (to keep the stream aligned) but may discard the block.

### 7.15 Extremes

**Direction:** Server → Client
**Packet type:** `8`
**Referenced by:** §6.4 (query phase)

Carries min/max values per column when the `extremes` setting is enabled. The block has exactly 2 rows: the first row contains the minimum of each column, the second row contains the maximum.

**Wire format: identical to Data (§7.11)** — a `table_name` string (always empty) followed by a Block. Only the packet type byte differs.

```
[VarUInt: 8]                     packet type (ServerPacket::Extremes)
[String: table_name]             always ""
[Block body]                     same format as §7.11; num_rows = 2
```

Not emitted unless the `extremes` setting is set on the query. Clients that don't use it should still decode to keep the stream aligned.

### 7.16 Log

**Direction:** Server → Client
**Packet type:** `10`
**Referenced by:** §6.4 (query phase)

Server-side log lines streamed during query execution. Useful for remote debugging and client-side progress display. Emitted only when the query has an active logs queue (controlled by the `send_logs_level` setting).

**Wire format: same envelope and body format as Data (§7.11).** The packet carries a normal Block whose `num_columns` is fixed at **8** and whose column schema is predefined (see below). Each log line is **one row** in the block, spanning all 8 columns columnar-style. A single Log packet can carry many log lines — `num_rows` varies per packet depending on how many the server is flushing.

```
[VarUInt: 10]                    packet type (ServerPacket::Log)
[String: table_name]             always ""
[BlockInfo]                      standard (§7.11)
[VarUInt: 8]                     num_columns — always 8 for Log
[VarUInt: num_rows]              number of log lines in this packet
[Column × 8]                     the 8 columns below, in this exact order
```

The 8 columns (following the standard Column wire format in §7.11 — name, type, has_custom_serialization, data):

| Column # | Name              | ClickHouse Type   | Description |
|----------|-------------------|-------------------|-------------|
| 1        | event_time        | DateTime          | Event timestamp (seconds since epoch) |
| 2        | event_time_microseconds | UInt32      | Microseconds component |
| 3        | host_name         | String            | Server hostname emitting the log |
| 4        | query_id          | String            | Query ID the log belongs to |
| 5        | thread_id         | UInt64            | OS thread ID |
| 6        | priority          | Int8              | Log level (Poco priority: 1=Fatal, 2=Critical, 3=Error, 4=Warning, 5=Notice, 6=Information, 7=Debug, 8=Trace) |
| 7        | source            | String            | Logger name (e.g., `"executeQuery"`, `"TCPHandler"`) |
| 8        | text              | String            | Log message text |

To extract log line `N` from the block, read `columns[i].data[N]` for each `i` in 0..7. Clients that ignore logs must still fully decode the block (all 8 columns × `num_rows` values) to keep the stream aligned.

### 7.17 ProfileEvents

**Direction:** Server → Client
**Packet type:** `14`
**Referenced by:** §6.4 (query phase)

Per-query performance counters (query execution metrics like bytes read from cache, network bytes, compression ratios, etc.).

**Wire format: same envelope and body format as Data (§7.11).** The packet carries a normal Block whose `num_columns` is fixed at **6** and whose column schema is predefined (see below). Each event is **one row** in the block. A single ProfileEvents packet can carry many events — `num_rows` varies per packet.

```
[VarUInt: 14]                    packet type (ServerPacket::ProfileEvents)
[String: table_name]             always ""
[BlockInfo]                      standard (§7.11)
[VarUInt: 6]                     num_columns — always 6 for ProfileEvents
[VarUInt: num_rows]              number of events in this packet
[Column × 6]                     the 6 columns below, in this exact order
```

The 6 columns (following the standard Column wire format in §7.11):

| Column # | Name        | ClickHouse Type | Description |
|----------|-------------|-----------------|-------------|
| 1        | host_name   | String          | Server hostname |
| 2        | current_time | DateTime       | Event timestamp |
| 3        | thread_id   | UInt64          | Thread ID |
| 4        | type        | Int8            | Event type: 1 = Increment (counter), 2 = Gauge (point-in-time value) |
| 5        | name        | String          | Event name (e.g., `"Query"`, `"QueryMemoryUsage"`, `"NetworkReceiveBytes"`) |
| 6        | value       | Int64 or UInt64 | Counter value or gauge reading. Type depends on event — see ClickHouse's profile events catalog. |

To extract event `N` from the block, read `columns[i].data[N]` for each `i` in 0..5. Clients that ignore profile events must still decode the block (all 6 columns × `num_rows` values) to keep the stream aligned.

### 7.18 TableColumns

**Direction:** Server → Client
**Packet type:** `11`
**Referenced by:** §6.4 (query phase)

Sent when the client needs column default values metadata, typically for INSERT queries that omit some columns (so the client knows the defaults). The payload is a human-readable textual description of the table schema.

| # | Field              | Type   | Role      | Description |
|---|--------------------|--------|-----------|-------------|
| 1 | external_table     | String | universal | External table name. Empty = main table. |
| 2 | columns_description | String | universal | Textual column definitions, e.g. `"id Int32, name String DEFAULT ''"`. Not a structured format on the wire — parse as a string. |

Clients that only issue SELECT queries rarely see this packet. Ignore or skip the body to keep the stream aligned.

---

## 8. Data Types & Column Encoding

This section documents how each ClickHouse data type is serialized within the `data` field of a Column (§7.11). The decoder reads the column's `type` string (also part of the Column header, §7.11), dispatches to the appropriate type decoder, and consumes exactly the bytes required by that type for `num_rows` values.

Type strings may carry parameters in parentheses (see §11.9). Base-type dispatch should strip the `(...)` suffix before matching; parameters may still be needed for size/scale/element-type decisions within the matched decoder.

Types are organized into four groups in order of increasing decoder complexity:

- **§8.1 Fixed-width types** — each row consumes a constant number of bytes. Single stream. No state, no version. Simplest category.
- **§8.2 Variable-length types** — each row consumes a variable number of bytes with a per-row length prefix. Single stream. No state, no version.
- **§8.3 Composite types (fixed shape)** — types built from one or more inner types, encoded as multiple streams per column (e.g., null-map + values, offsets + values). Wire format is stable and unversioned; shape is fully determined by the type string at decode time.
- **§8.4 Versioned / stateful types** — types whose wire format has evolved over time, carry a serialization-version prefix, and may maintain cross-block state. Typically also feature runtime-varying per-row sub-types.

A client that supports §8.1 and §8.2 can handle most simple queries. §8.3 adds support for nulls, arrays, and structured data. §8.4 is needed only for advanced analytics types; implementation complexity jumps significantly at this boundary.

### 8.1 Fixed-width types

Fixed-width types encode each value as a constant number of bytes. A column of a fixed-width type with `N` bytes per value and `M` rows occupies exactly `N * M` bytes on the wire, concatenated with no separators, length prefixes, or padding.

All multi-byte integer types are encoded **little-endian**. Signed integers use **two's complement**. There is no per-column header beyond the generic Column header in §7.11 (name, type string, has_custom_serialization byte).

#### Byte layout summary

| Type string    | Bytes per value | Logical value                              | Wire encoding |
|----------------|-----------------|--------------------------------------------|---------------|
| `UInt8`        | 1               | Unsigned 8-bit integer                     | Raw byte      |
| `UInt16`       | 2               | Unsigned 16-bit integer                    | Little-endian |
| `UInt32`       | 4               | Unsigned 32-bit integer                    | Little-endian |
| `UInt64`       | 8               | Unsigned 64-bit integer                    | Little-endian |
| `Int8`         | 1               | Signed 8-bit integer, two's complement     | Raw byte      |
| `Int32`        | 4               | Signed 32-bit integer, two's complement    | Little-endian |
| `Int64`        | 8               | Signed 64-bit integer, two's complement    | Little-endian |
| `DateTime`     | 4               | Unix timestamp in seconds since epoch      | Little-endian UInt32 |
| `DateTime(tz)` | 4               | Same as `DateTime`; timezone is metadata   | Little-endian UInt32 |
| `Enum8`        | 1               | Signed 8-bit integer (variant index)       | Raw byte      |

#### 8.1.1 Integer types

`UInt8`, `UInt16`, `UInt32`, `UInt64`, `Int8`, `Int32`, `Int64` are direct binary encodings of integer values. Decoders read `bytes_per_value * num_rows` bytes and interpret them according to the type.

**Example** — a `UInt32` column with values `[1, 256, 65536]` (3 rows = 12 bytes):

```
01 00 00 00   row 0: 1
00 01 00 00   row 1: 256
00 00 01 00   row 2: 65536
```

**Example** — an `Int32` column with values `[-1, 42]` (2 rows = 8 bytes):

```
FF FF FF FF   row 0: -1
2A 00 00 00   row 1: 42
```

#### 8.1.2 DateTime

`DateTime` is wire-compatible with `UInt32` — each value is a Unix timestamp in seconds since `1970-01-01 00:00:00 UTC`, encoded as a little-endian UInt32 (4 bytes).

The type may appear with or without a timezone parameter:

- `DateTime` — implicit server default timezone.
- `DateTime('UTC')`, `DateTime('America/New_York')`, etc. — explicit timezone.

The timezone affects how the server and client render the timestamp as text; it is **not** part of the wire value. Two `DateTime` columns with different timezone parameters have identical byte representations for the same instant in time.

Decoders dispatch on the base type name `DateTime` (§11.9) and ignore the timezone parameter for byte-level decoding. The timezone may be preserved alongside the decoded values if the client wants to honor it for display or conversion.

**Example** — `DateTime('UTC')` value `2024-03-15 14:30:00 UTC` (Unix timestamp `1710513000`):

```
A8 84 F4 65   Little-endian UInt32 = 0x65F484A8 = 1710513000
```

#### 8.1.3 Enum8

`Enum8` is wire-compatible with `Int8` — each row is a single signed byte representing the variant's integer value (range: -128 to 127). The human-readable variant names live in the **type string**, not in the column data.

The type string carries the full variant mapping, for example:

```
Enum8('active' = 1, 'inactive' = 2, 'banned' = -1)
```

Clients that care about the human-readable name must parse the type string; clients that only need the numeric value can treat the column as a plain `Int8`. The decoder's byte-level work is identical to `Int8`.

**Example** — an `Enum8('active' = 1, 'inactive' = 2)` column with values `[active, inactive, active]` (3 rows = 3 bytes):

```
01 02 01
```

> **Note:** `Enum16` (a 2-byte counterpart) is also used by some ProfileEvents columns and follows the same principle — wire-compatible with `Int16`. `Enum16` is declared here for completeness but not yet implemented in the reference decoder. When implemented, it will use little-endian 2-byte signed integers.

#### 8.1.4 Fixed-width types not yet implemented

These types are fixed-width and will use the same "N bytes per row, concatenated" pattern, but are not yet part of the reference implementation:

- `Int16` — 2 bytes, little-endian signed. Currently aliased to `Enum16` only.
- `Float32`, `Float64` — 4 and 8 bytes, IEEE 754 binary float, little-endian.
- `Bool` — 1 byte, `0x00` = false, `0x01` = true. Internally a domain over `UInt8`.
- `Date` — 2 bytes, little-endian UInt16 (days since `1970-01-01`).
- `Date32` — 4 bytes, little-endian Int32 (days since `1970-01-01`, allows pre-1970 dates).
- `DateTime64(scale)` — 8 bytes, little-endian Int64 (ticks; scale in type string controls subsecond precision; 3 = ms, 6 = µs, 9 = ns).
- `UUID` — 16 bytes, but with a historical quirk: transmitted as two little-endian UInt64 halves, **each byte-swapped**. See §11.x (planned) for the exact byte reordering.
- `IPv4` — 4 bytes, raw IPv4 address (typically little-endian on the wire, but this is domain-specific).
- `IPv6` — 16 bytes, raw IPv6 address in network byte order.
- `Int128`, `UInt128`, `Int256`, `UInt256` — 16 / 32 bytes, little-endian.
- `Decimal32(S)`, `Decimal64(S)`, `Decimal128(S)`, `Decimal256(S)` — 4 / 8 / 16 / 32 bytes, little-endian signed integer representing the decimal value scaled by `10^S`. The scale `S` is in the type string.

### 8.2 Variable-length types

#### 8.2.1 String

**Type string:** `String`
**In-memory model:** a sequence of arbitrary byte strings (not necessarily UTF-8 on the wire, though most values are).

A `String` column is a sequence of `num_rows` length-prefixed byte strings, concatenated end-to-end with no padding. Each value is:

```
[VarUInt: byte_length]
[byte_length bytes: raw value]
```

There are no separators, no row boundaries beyond the length prefixes, and no type-level state. Empty strings are a single `0x00` byte (VarUInt length 0 followed by zero bytes). Strings may contain any byte values including embedded NUL (`0x00`).

Although ClickHouse's `String` type is commonly used for UTF-8 text, the wire representation is byte-oriented and does not require UTF-8 validity. Clients that decode into a UTF-8-constrained type (e.g., language-native string types) should either validate on decode or expose the raw bytes for the caller to handle.

**Total bytes consumed by a String column:** sum of `(varuint_size(len_i) + len_i)` for `i` in `0..num_rows`.

**Wire example** — a column of 3 strings `["ab", "", "c"]`:

```
02 61 62      row 0: length 2, "ab"
00            row 1: length 0, empty
01 63         row 2: length 1, "c"
```

Total: 6 bytes of column data.

#### 8.2.2 FixedString(N)

**Type string:** `FixedString(N)` where `N` is a positive integer (e.g., `FixedString(16)`).
**In-memory model:** a sequence of byte strings all of exactly `N` bytes.

A `FixedString(N)` column is exactly `N * num_rows` raw bytes — no length prefixes, no separators. The decoder must parse `N` from the type string and consume that many bytes per row.

When a value shorter than `N` bytes is inserted by SQL (`CAST('abc' AS FixedString(5))`), the server right-pads it with NUL bytes (`0x00`) to the declared length. These padding bytes are part of the stored value and are sent on the wire. Clients receive the padded bytes as-is; trimming is a client-side concern.

Unlike `String`, `FixedString(N)` values are **byte-array-like**, not text-like — they are typically used for fixed-width identifiers, IP address-style bytes, hash digests, etc. Do not assume UTF-8.

**Total bytes consumed by a FixedString(N) column:** `N * num_rows`.

**Wire example** — a column of 2 `FixedString(3)` values `["abc", "de\0"]`:

```
61 62 63      row 0: 3 bytes, "abc"
64 65 00      row 1: 3 bytes, "de" followed by NUL padding
```

Total: 6 bytes of column data.

**Type-string parsing:**

- Strip the `(...)` suffix to get the base type (`FixedString`).
- Parse the integer between the parentheses to get `N`.
- Reject malformed variants (empty parentheses, non-numeric contents, missing closing paren).

Comparison between String and FixedString:

| Property | `String` | `FixedString(N)` |
|----------|----------|------------------|
| Per-row length prefix | Yes (VarUInt) | No |
| Row size | Variable | Exactly `N` bytes |
| Total column bytes | Variable | `N * num_rows` |
| NUL-byte padding | Not applicable (length-prefixed) | Right-padded by server for short values |
| UTF-8 expected | Typically yes (not enforced) | No (treat as raw bytes) |
| Type parameter | None | Required integer `N` |

### 8.3 Composite types (fixed shape)

Composite types are built by wrapping one or more inner types. They share a common wire model: **multiple streams per column**. A single logical column on the wire is encoded as two or more independently-read sequences of bytes, concatenated: for example, `Nullable(UInt32)` is a null-map stream followed by a values stream.

Unlike §8.4, types in this group have a **stable, unversioned wire format** and no cross-block state. The full shape of the column is known from the type string (e.g., `Array(String)`, `Nullable(UInt32)`, `Tuple(UInt8, String)`) — the decoder statically derives the streams it must read.

The structural properties shared by all types in this group:

- **Fixed shape per schema.** The structure is determined entirely by the type string at decode time. `Array(UInt32)` always has the same stream layout regardless of block.
- **No version prefix.** The stream layout is stable across ClickHouse releases.
- **No cross-block state.** Each block is fully self-describing; a decoder never needs information from a previous block to decode the current one.
- **Recursive.** Inner types of a composite may themselves be any type, including other composites. `Array(Nullable(String))` is composed of `Array(...)` with a `Nullable(String)` value stream.

#### 8.3.1 Nullable(T)

**Type string:** `Nullable(InnerType)` where `InnerType` is any ClickHouse type. Examples: `Nullable(UInt32)`, `Nullable(String)`, `Nullable(FixedString(16))`, `Nullable(DateTime('UTC'))`.

**Semantic model:** each row holds either a value of the inner type or a SQL NULL. Nullability is orthogonal to the element type — any row may independently be null or present.

**Wire layout:** two concatenated streams, null-map first:

```
[null-map stream]   num_rows × UInt8
[values stream]     inner type's encoding for num_rows values
```

**Stream 1 — null-map:** exactly `num_rows` bytes, one per row. Each byte indicates whether the corresponding row is null.

| Byte value | Meaning |
|------------|---------|
| `0x00`     | Value is present at this row. Read the value from the values stream at the corresponding position. |
| non-zero (`0x01` canonical) | Value at this row is NULL. The bytes at the corresponding position in the values stream are a placeholder and should be ignored. |

**Stream 2 — values:** the inner type's standard encoding for **all** `num_rows` rows, including the null positions. The values at null positions are placeholder bytes and may be anything the server chose to emit (typically zero-initialized or empty). The decoder must still read and consume them to advance the stream position correctly; callers must consult the null-map before interpreting any individual value.

This "always emit `num_rows` values" invariant is what allows positional indexing into the values stream without computing a per-row offset. It is a deliberate space-for-simplicity tradeoff — a null-heavy column wastes some bytes in the values stream, but decoders can seek to row `N` by simple arithmetic.

**Placeholder values by inner type:**

| Inner type family | Placeholder at null position |
|-------------------|-----------------------------|
| Fixed-width (UInt/Int/Float/DateTime/UUID/etc.) | Zero-initialized bytes of the appropriate width. |
| `String` | Empty string — a single `0x00` byte (VarUInt length 0, zero bytes of data). |
| `FixedString(N)` | N zero bytes. |
| `Array(T)` | Empty array — offsets stream advances by zero at the null row. |
| `Tuple(T1, T2, ...)` | Each element uses its own default placeholder. |

Senders (clients on INSERT) may write any bytes at null positions; servers are expected to write deterministic defaults. Decoders must not rely on any specific placeholder value.

**Nesting:** `Nullable(T)` may be used inside other composites. For example:

- `Array(Nullable(UInt32))` — an array column where individual elements may be null.
- `Tuple(Nullable(String), UInt8)` — a tuple where the first element is nullable.

`Nullable(Nullable(T))` is not allowed — the server rejects this type. Nullability is not composable with itself.

**Byte-level example:** a `Nullable(UInt8)` column with three rows `[5, NULL, 9]`:

```
00 01 00                         null-map: present, null, present
05 00 09                         values:   5, placeholder (0), 9
```

Total: 6 bytes of column data. Framed as part of the Column wire layout (§7.11):

```
01 'x'                           Column.name = "x"
0F 'N' 'u' 'l' 'l' 'a' 'b' 'l'   Column.type = "Nullable(UInt8)" (15 chars)
   'e' '(' 'U' 'I' 'n' 't' '8'
   ')'
00                               has_custom_serialization = 0
00 01 00                         null-map
05 00 09                         values
```

Total column bytes on the wire: 26.

**Byte-level example — `Nullable(String)` with three rows `["hello", NULL, "world"]`:**

```
00 01 00                         null-map
05 'h' 'e' 'l' 'l' 'o'           row 0: "hello"
00                               row 1: placeholder (empty string)
05 'w' 'o' 'r' 'l' 'd'           row 2: "world"
```

Total: 15 bytes of column data.

**Decoder algorithm:**

1. Read `num_rows` bytes into the null-map buffer.
2. Recursively decode the inner type's column data for `num_rows` values.
3. Expose an accessor that, given a row index `N`, returns either the decoded value at position `N` or a null marker, determined by the null-map byte at position `N`.

**Encoder algorithm:**

1. Write the `num_rows` null-map bytes.
2. Write the inner type's encoding for all `num_rows` values, filling null positions with the default placeholder for the inner type.

**Common mistakes:**

- **Writing values first, then the null-map.** The spec order is null-map first. Swapping produces a stream that decodes to garbage.
- **Writing fewer values than `num_rows`** (e.g., skipping null positions in the values stream). The values stream must have exactly `num_rows` values.
- **Relying on the placeholder bytes as real data.** Servers may emit any bytes at null positions; decoders must treat them as unspecified.

#### 8.3.2 Array(T)

**Type string:** `Array(InnerType)` where `InnerType` is any ClickHouse type. Examples: `Array(UInt32)`, `Array(String)`, `Array(Nullable(UInt32))`, `Array(Array(UInt8))`.

**Semantic model:** each row holds a variable-length sequence of values of the inner type. Rows can have any number of elements (including zero); different rows in the same column may have different lengths.

**Wire layout:** two concatenated streams, offsets first:

```
[offsets stream]    num_rows × UInt64 LE
[values stream]     inner type's encoding for (offsets[num_rows - 1]) values
```

**Stream 1 — offsets:** exactly `num_rows` values, each a little-endian `UInt64` (8 bytes). Each offset is the **cumulative end position** in the values stream after that row's elements.

For row `N` (zero-indexed):
- Element **start** index (in the values stream) = `offsets[N - 1]` (or `0` when `N == 0`).
- Element **end** index (exclusive) = `offsets[N]`.
- Row `N`'s element count = `offsets[N] - offsets[N - 1]` (or `offsets[0]` when `N == 0`).

The last offset `offsets[num_rows - 1]` equals the **total number of elements** across all rows — this is what the decoder uses to know how many values to read from the values stream.

**Stream 2 — values:** the inner type's standard encoding for all `offsets[num_rows - 1]` values, concatenated end-to-end. No separators or row boundaries; the offsets stream is the only source of row-boundary information.

**Key invariants:**

1. Offsets are **monotonic non-decreasing**: `offsets[i] >= offsets[i - 1]` for all `i > 0`. Equal consecutive offsets mean an empty row (e.g., `[3, 3, 5]` — row 1 is empty).
2. The values stream contains **exactly `offsets[num_rows - 1]` values** — no more, no fewer.
3. An empty column (`num_rows == 0`) has no offsets stream and no values stream — zero bytes.

Decoders must validate invariant 1 (monotonicity) and fail loudly on violations; a non-monotonic offsets stream indicates corruption or a server bug and the subsequent decode would produce garbage.

**Why cumulative offsets (not per-row lengths)?**

- **O(1) random access.** Row `N`'s elements are at `values[offsets[N-1]..offsets[N]]` — two reads, no scan required. Per-row lengths would require summing from row 0.
- **Total element count is free.** `offsets[num_rows - 1]` is immediately the total, which the decoder needs up-front to size the inner stream's read.
- **Matches in-memory layout.** ClickHouse stores arrays internally as (cumulative offsets, flat values) — the wire format is a direct serialization with no transformation.

**Nesting: `Array(Array(T))`.** Each layer has its own offsets stream over the flat element count of the layer beneath it. Concretely, for rows `[[[1,2]], [], [[3], [4,5]]]`:

- **Outer** `Array(Array(UInt32))`:
  - `num_rows = 3` (three logical rows).
  - `offsets = [1, 1, 3]` — row 0 has 1 inner-array, row 1 has 0, row 2 has 2. Total inner-arrays: 3.
- **Middle** `Array(UInt32)` (decodes `3` rows because the outer's last offset is `3`):
  - `offsets = [2, 3, 5]` — first inner-array has 2 elements, second has 1, third has 2. Total elements: 5.
- **Innermost** `UInt32` (decodes `5` values because the middle's last offset is `5`):
  - `values = [1, 2, 3, 4, 5]`.

Total bytes: 24 (outer offsets) + 24 (middle offsets) + 20 (values) = 68 bytes.

**Composition with Nullable.** `Array(Nullable(T))` is legal and common. The outer Array encodes as usual; the inner becomes a `Nullable(T)` column whose `num_rows` equals the outer's total element count. Each individual element of an array may independently be null.

`Nullable(Array(T))` is also legal — the outer row may be entirely null (distinct from an empty array). The inner values stream at a null outer row typically encodes as an empty array (same cumulative offset as the previous row).

**Byte-level example — `Array(UInt32)` with `[[10, 20, 30], [], [40, 50]]`:**

```
Offsets (3 × UInt64 LE = 24 bytes):
03 00 00 00 00 00 00 00      offsets[0] = 3
03 00 00 00 00 00 00 00      offsets[1] = 3
05 00 00 00 00 00 00 00      offsets[2] = 5

Values (5 × UInt32 LE = 20 bytes):
0A 00 00 00                  10
14 00 00 00                  20
1E 00 00 00                  30
28 00 00 00                  40
32 00 00 00                  50
```

Total: 44 bytes of column data.

Framed as part of the Column wire layout (§7.11):

```
03 'a' 'r' 'r'                          Column.name = "arr"
0D 'A' 'r' 'r' 'a' 'y'                  Column.type = "Array(UInt32)" (13 chars)
   '(' 'U' 'I' 'n' 't' '3' '2' ')'
00                                       has_custom_serialization = 0
[24 bytes of offsets as above]
[20 bytes of values as above]
```

Total column bytes on the wire: 62.

**Byte-level example — `Array(String)` with `[["a", "bb"], []]`:**

```
Offsets (2 × UInt64 LE = 16 bytes):
02 00 00 00 00 00 00 00      offsets[0] = 2
02 00 00 00 00 00 00 00      offsets[1] = 2 (empty row)

Values (2 strings, 4 bytes total):
01 'a'                        row's first string: "a"
02 'b' 'b'                    row's second string: "bb"
```

Total: 20 bytes.

**Decoder algorithm:**

1. Read `num_rows × 8` bytes as `num_rows` `UInt64` values → offsets vector.
2. Validate offsets are monotonic non-decreasing; fail on violation.
3. Compute `total_elements = offsets.last().unwrap_or(&0)` (or `0` if `num_rows == 0`).
4. Recursively decode the inner type with `total_elements` as the row count.
5. Expose accessors that, given a row index `N`, return the slice of values at `offsets[N-1]..offsets[N]` (with `offsets[-1] = 0`).

**Encoder algorithm:**

1. Build cumulative offsets while iterating logical rows: `offsets.push(total_so_far)` after each row.
2. Write the `num_rows × 8` bytes of offsets.
3. Write the inner type's encoding for all accumulated values.

**Invariant when constructing in memory:** `inner.row_count() == offsets.last().unwrap_or(&0) as usize`. Implementations validate this at encode time (see §12 implementation notes).

**Common mistakes:**

- **Passing `num_rows` instead of `total_elements` to the inner decoder.** The outer Array has `num_rows` rows, but the inner values stream has `total_elements` values. The inner decode must be told the latter.
- **Writing per-row lengths instead of cumulative end-offsets.** Lengths would be `[3, 0, 2]`; offsets are `[3, 3, 5]`. These look similar for the first row but diverge quickly.
- **Forgetting that empty rows produce duplicate offsets.** A row with zero elements does not advance the offset; an offsets stream like `[5, 5, 5]` means three empty rows.
- **Computing total from `num_rows × avg_len`.** The total element count is `offsets.last()`, not derivable from `num_rows` alone.

#### 8.3.3 Tuple(T1, T2, ...)

**Type string:** `Tuple(T1, T2, ..., Tn)` where each `Ti` is any ClickHouse type. Examples: `Tuple(UInt32, String)`, `Tuple(Int32)`, `Tuple(Array(UInt32), String)`, `Tuple(UInt8, Tuple(Int32, String))`.

ClickHouse also supports **named tuples** with the syntax `Tuple(a UInt32, b String)`. The names are metadata only — they do not affect the wire format. (This client treats element names as part of the type string for now and does not expose them; see §11.16.)

**Semantic model:** each row holds a fixed-arity heterogeneous record — exactly one value of `T1`, one of `T2`, …, one of `Tn`. The arity is fixed by the type string; every row has the same shape. Conceptually a Tuple column is *N* parallel columns, one per element type, sharing the same row count.

**Wire layout:** *N* concatenated streams, one per element type, in declaration order:

```
[stream for T1]    inner type T1's encoding for num_rows values
[stream for T2]    inner type T2's encoding for num_rows values
 ...
[stream for Tn]    inner type Tn's encoding for num_rows values
```

Each stream is the standard encoding of `Ti` for **`num_rows` values** — the same row count as the outer column. There is no length prefix, no offsets stream, no separators between streams. Decoders rely on knowing `num_rows` from the enclosing Block (§7.11) and the type string from the column header.

**Key invariants:**

1. The Tuple has at least one element (`n >= 1`). ClickHouse rejects empty tuples at type-parse time.
2. Every element stream encodes exactly `num_rows` values of its declared type.
3. An empty column (`num_rows == 0`) writes zero bytes — every per-element stream is empty.

There is no decoder validation specific to Tuple — each element decode succeeds or fails on its own. Tuple is a structural container; correctness is delegated to its element types.

**Why per-element streams (rather than per-row interleaving)?**

ClickHouse's columnar in-memory layout stores each tuple element as its own column. The wire format mirrors that layout exactly — element `i`'s on-wire bytes are the same bytes ClickHouse holds in memory for that sub-column. Decoders can demux a Tuple straight into separate columns without reshuffling row-major data.

A row-major encoding (`row0_T1 row0_T2 row1_T1 row1_T2 ...`) would require both sides to interleave/de-interleave on every block, and would defeat per-element vectorised reads.

**Nesting: `Tuple(Tuple(...), ...)`.** A nested tuple is just one element whose type is itself a Tuple. The outer Tuple's stream for that element is the inner Tuple's full multi-stream encoding for `num_rows` values. There are no extra layers of bookkeeping.

For `Tuple(UInt8, Tuple(Int32, String))` with 2 rows `(1, (100, 'x'))`, `(2, (200, 'y'))`:
- Element 0 stream: `Uint8` encoding of `[1, 2]`.
- Element 1 stream: `Tuple(Int32, String)` encoding of `[(100, 'x'), (200, 'y')]`, which itself decomposes into:
  - Inner element 0 stream: `Int32` encoding of `[100, 200]`.
  - Inner element 1 stream: `String` encoding of `["x", "y"]`.

**Composition with Array, Nullable.** `Tuple` composes freely with the other composites. Each element's stream is whatever that element type would encode for `num_rows` values:

- `Tuple(Array(UInt32), String)` with 2 rows `([1,2,3], "hi")`, `([4], "bye")`:
  - Element 0: `Array(UInt32)` encoding for 2 rows → offsets `[3, 4]` (16 bytes) + values `[1, 2, 3, 4]` (16 bytes).
  - Element 1: `String` encoding for 2 rows → `[2 'h' 'i'] [3 'b' 'y' 'e']` (8 bytes).
- `Tuple(Nullable(UInt32), String)` with 1 row `(NULL, 'present')`:
  - Element 0: `Nullable(UInt32)` for 1 row → null-map `[1]` (1 byte) + placeholder `Uint32` value (4 bytes).
  - Element 1: `String` for 1 row → `[7 'p' 'r' 'e' 's' 'e' 'n' 't']` (8 bytes).

**Byte-level example — `Tuple(UInt8, UInt8)` with 3 rows `(1,4), (2,5), (3,6)`:**

```
Element 0 stream (3 × UInt8 = 3 bytes):
01 02 03

Element 1 stream (3 × UInt8 = 3 bytes):
04 05 06
```

Total: 6 bytes of column data. Note the ordering — it is **not** row-major `01 04 02 05 03 06`. Reading the raw bytes back in declaration order yields `[1, 2, 3]` for element 0 and `[4, 5, 6]` for element 1.

Framed as part of the Column wire layout (§7.11):

```
01 't'                                  Column.name = "t"
13 'T' 'u' 'p' 'l' 'e' '('              Column.type = "Tuple(UInt8, UInt8)" (19 chars)
   'U' 'I' 'n' 't' '8' ',' ' '
   'U' 'I' 'n' 't' '8' ')'
00                                       has_custom_serialization = 0
01 02 03                                 element 0: UInt8 stream
04 05 06                                 element 1: UInt8 stream
```

Total column bytes on the wire: 28.

**Byte-level example — `Tuple(UInt32, String)` with 2 rows `(10, "a")`, `(20, "bb")`:**

```
Element 0 stream (2 × UInt32 LE = 8 bytes):
0A 00 00 00                  10
14 00 00 00                  20

Element 1 stream (2 strings, 5 bytes total):
01 'a'                       "a"
02 'b' 'b'                   "bb"
```

Total: 13 bytes of column data.

**Decoder algorithm:**

1. Parse the type string to extract element types `T1, T2, ..., Tn`. **Do not split naively on `,`** — element types may themselves contain commas inside parentheses (`Tuple(Int32, String)` as an element, or `Map(String, UInt32)`). Use a depth-aware scanner: increment on `(`, decrement on `)`, split only when depth `== 0`. Reject if depth doesn't end at `0` (unbalanced parens).
2. For each `Ti` in order, recursively decode `num_rows` values of type `Ti`.
3. Return the *N*-element vector of decoded element columns.

**Encoder algorithm:**

1. Verify all element columns have the same row count.
2. For each element column in declaration order, write its standard encoding for `num_rows` values.

No length prefix, no element separator — bytes are written end-to-end and the receiver demuxes using the type string.

**Invariant when constructing in memory:** every element column has the same `row_count()`, and that common value is the Tuple column's logical row count. Implementations must validate this at encode time and recurse into each element to validate nested invariants (see §11.16).

**Common mistakes:**

- **Splitting the type string naively on commas.** `Tuple(Tuple(Int8, Int32), String)` contains commas at multiple paren depths. A naive `split(",")` produces the garbage list `["Tuple(Int8", "Int32)", "String"]`. Use depth-aware splitting.
- **Encoding row-major.** Writing `(row0_T1, row0_T2, row1_T1, ...)` instead of `(all T1 values, all T2 values)` corrupts the stream. The values stream for Tuple is element-major.
- **Treating the inner element vector length as `row_count`.** `ColumnData::Tuple(Vec<...>)` holds *N* element columns; `vec.len()` is *N* (the arity), not the row count. The row count lives inside each element.
- **Skipping recursion in `validate()`.** Checking that all element row counts agree is necessary but not sufficient — a `Tuple(Array(...), ...)` whose Array has non-monotonic offsets must also be rejected before encode. Recurse into each element.
- **Using `decode(r, dt, 1)` for inner elements.** Each element's stream contains `num_rows` values, not 1 — pass the outer `num_rows` down unchanged.

#### 8.3.4 Other composite types (not yet specified)

> **Not yet specified in this document:** `Map(K, V)`, `Nested(...)`.
>
> Planned wire format sketches (to be fleshed out):
> - **`Map(K, V)`** — equivalent to `Array(Tuple(K, V))`; shares `Array`'s offsets stream + a paired `Tuple(K, V)` values stream.
> - **`Nested(name1 T1, ...)`** — syntactic sugar that expands to several parallel `Array(T_i)` columns in the block's column list.

### 8.4 Versioned / stateful types

This group covers the most complex types in the protocol. Each of these types:

- Begins its column data with a **serialization-version prefix** that declares which variant of the wire encoding follows.
- May use **multiple streams** (like §8.3) but with a state prefix preceding them.
- May maintain **cross-block state** — e.g., a dictionary that accumulates values across blocks, or a path set that grows as new paths appear in later blocks.
- Typically supports **runtime-varying sub-types** — different rows may hold values of different concrete types (the sub-type is chosen per row at query time, not fixed by the schema).

Implementing support for these types is an order of magnitude more work than §8.3. A client targeting simple analytical queries can defer this section.

#### 8.4.1 Serialization version: concept and purpose

A **serialization version** is a per-type, on-wire version number that declares which variant of that type's encoding the sender is using.

**Where it sits on the wire.** Types in this group have a *state prefix* that precedes the row values. The serialization version is the first thing in that prefix, so the decoder reads it and dispatches to the right parser for the rest of the column.

**Why it exists.** These types' wire formats have evolved over time — fields added, redundant parameters removed, optimizations introduced. The version prefix lets senders and receivers agree on which variant is in use, independent of the connection-level protocol version (§4).

**Relationship to the protocol version.** The serialization version is **distinct** from the protocol version:

| Dimension                     | Protocol version (§4)            | Serialization version (this section) |
|-------------------------------|----------------------------------|--------------------------------------|
| Scope                         | Connection-wide                  | Per-type, per-column                 |
| Negotiated                    | Yes, at handshake                | No — sender writes, receiver reads   |
| Controls                      | Which packet-level features are active | Which wire variant of one type    |
| Tied to ClickHouse release?   | No (evolves independently)       | No (evolves independently)           |
| Mandatory to read?            | Yes                              | Yes, for each versioned column       |

**Common encoding.** Most versioned types write the version as a **little-endian UInt64** immediately before any other state-prefix data. A few use VarUInt or UInt8 — the exact width is per-type.

**What versions are not.** They are not ClickHouse release numbers, not protocol version numbers, and not necessarily contiguous (e.g., `Dynamic` skips value `0` and uses values `1`, `2`, `3`, `4` non-monotonically in semantic order).

**Decoder obligations.**
- Read the version first.
- Reject unknown values — they indicate a newer sender format the current decoder does not understand. Failing loudly is safer than guessing, because mis-parsing corrupts every subsequent byte.
- Dispatch to the correct sub-format parser for the rest of the column.

#### 8.4.2 Serialization version value reference

| Type | Field width | Value | Name | Meaning |
|---|---|---|---|---|
| **Object** (base for JSON) | UInt64 LE | `0` | `V1` | Original encoding. Includes `max_dynamic_paths` parameter and a list of dynamic paths. |
| | | `1` | `STRING` | Native-format compatibility mode — Object transmitted as a single `String` column containing JSON text. |
| | | `2` | `V2` | V1 layout minus the `max_dynamic_paths` parameter (server found it unnecessary on read). |
| | | `3` | `FLATTENED` | Native-format compatibility mode — flattened path representation for clients that cannot handle the nested Object structure. |
| | | `4` | `V3` | V2 layout plus a shared-data serialization version sub-field and a statistics flag. |
| **Object shared data** (sub-stream used in Object `V3`) | VarUInt | `0` | `MAP` | Shared data encoded as `Map(String, String)`. |
| | | `1` | `MAP_WITH_BUCKETS` | Same as `MAP` but split into N buckets for scan efficiency. |
| | | `2` | `ADVANCED` | Compact granule format with separate streams for paths / marks / metadata. |
| **Dynamic** | UInt64 LE | `1` | `V1` | Original encoding. Includes `max_dynamic_types` parameter and a list of runtime variant types. |
| | | `2` | `V2` | V1 minus the `max_dynamic_types` parameter. |
| | | `3` | `FLATTENED` | Native-format compatibility mode for clients that cannot decode the full Dynamic structure. |
| | | `4` | `V3` | V2 plus binary-encoded variant type names and empty-statistics support. |
| **Variant** discriminators mode | UInt64 LE | `0` | `BASIC` | Every row's discriminator is written literally. |
| | | `1` | `COMPACT` | If all rows in a granule share one discriminator, only a single value + granule marker is written. |
| **Variant** granule format (when mode is `COMPACT`) | UInt8 | `0` | `PLAIN` | This granule has heterogeneous discriminators — write them all. |
| | | `1` | `COMPACT` | This granule has one discriminator for all its rows. |
| **LowCardinality** key serialization | Int64 | `1` | `sharedDictionariesWithAdditionalKeys` | Only version currently defined — shared dictionary with per-block additional keys (dictionary deltas). |
| **JSON-as-String** fallback (when `output_format_native_write_json_as_string` is enabled) | UInt64 LE | `1` | `JSONStringSerializationVersion` | JSON column arrives as a `String` column preceded by this version prefix. Any other value means the server is sending the real binary JSON format, which requires the full Object/Dynamic implementation. |

Notes on the table:

- **Values aren't always contiguous.** `Dynamic` has values `1`, `2`, `3`, `4` with `V3` at `4` and `FLATTENED` at `3`. Don't assume `0..N` is defined or that higher numbers are newer.
- **Native-format-only values.** `Object::STRING`, `Object::FLATTENED`, `Dynamic::FLATTENED` exist for Native protocol wire use (compatibility with clients that don't implement full Object/Dynamic); they do not appear in MergeTree on-disk storage.
- **LowCardinality is barely versioned.** The only defined value is `1`. The versioning scaffolding is in place should the encoding ever change.
- **`V3` is primarily on-disk.** `Object::V3` (value `4`) and `Dynamic::V3` (value `4`) are the storage-internal formats with richer metadata (shared-data sub-version, statistics, binary-encoded variant names). Clients consuming the native TCP protocol typically see `FLATTENED` (value `3`) instead, which carries equivalent column data without the storage-specific metadata. An implementation targeting only native-protocol traffic can reasonably skip the `V3` branch.

#### 8.4.2.1 Client implementation tiers for JSON

Three progressively-harder strategies for supporting JSON in a client, corresponding to the subset of Object versions the client decodes:

**Tier 1 — String-only (simplest).** Force the server to downgrade JSON to text by setting `output_format_native_write_json_as_string = 1` in every query. Decode only version `1` (`STRING`). Reject any other value with a clear error. JSON arrives as a single UTF-8 text payload per row; users parse it themselves (or the client wraps it in a convenience type). Implementation effort: a few hours on top of existing `String` support.

**Tier 2 — String + FLATTENED (matches clickhouse-go v2).** Decode versions `0` (legacy `V1`; auto-upgrade to FLATTENED on read), `1` (`STRING`), and `3` (`FLATTENED`). FLATTENED requires supporting the full `Dynamic` type underneath, which in turn requires `Variant`. A client at this tier delivers structured JSON — typed paths exposed as individual column-like access. Implementation effort: significant (thousands of lines of code to implement FLATTENED + Dynamic + Variant correctly).

**Tier 3 — Full (`V3` support).** Also decodes versions `2` (`V2`) and `4` (`V3`). `V3` adds the `shared_data` sub-stream with its own version enum (§8.4.2), plus statistics and extended metadata. In practice this tier is only useful for clients that read raw on-disk MergeTree parts; the native TCP server does not emit `V3` to remote clients in typical operation.

Most production Go/Rust/Python clients sit at Tier 2. The Tier 1 fallback remains useful as a simpler path when full JSON support is not required.

#### 8.4.3 Types in this group

> **Not yet specified in this document.** The following types belong to this group but their full wire format is not yet documented:
>
> - **`LowCardinality(T)`** — dictionary-encoded column with cross-block state (accumulating additional keys per block). Simplest of the versioned types; only one version defined.
> - **`Variant(T1, T2, ...)`** — discriminated union. Each row has a discriminator byte/short indicating which alternative holds the value; then per-alternative streams carry the actual values.
> - **`Dynamic`** — runtime-typed column. Each row's type is chosen from a set of variants that can grow across blocks. Built from `Variant` plus a structure prefix.
> - **`Object`** (underlying `JSON`) — tree of dynamically-discovered paths, each path a `Dynamic` column. Complex due to the multi-layer composition (`Object` over `Dynamic` over `Variant`).
> - **`JSON`** — thin wrapper over `Object`. Clients may bypass the full implementation by setting `output_format_native_write_json_as_string = 1`, in which case JSON arrives as a `String` column with the `JSONStringSerializationVersion` prefix — see §8.4.2.
>
> Recommended implementation order for clients: `LowCardinality` → `Variant` → `Dynamic` → `Object` / `JSON`.

### 8.5 Types not yet categorized

A catch-all for types recognized by the server that aren't documented above:

- **`Decimal(P, S)`**, **`Decimal32/64/128/256`** — fixed-point decimal numerics. Will fall under §8.1 (fixed-width).
- **`AggregateFunction`**, **`SimpleAggregateFunction`** — serialized aggregation state. Structurally similar to §8.3 composites.
- **`Interval`** — calendar/time interval. Fixed-width.
- **Geo types** — `Point`, `Ring`, `Polygon`, `MultiPolygon`. Composites built on `Tuple` and `Array`.
- **`Int16`, `UInt16`**, **`Int128`, `UInt128`**, **`Int256`, `UInt256`**, **`Float32`, `Float64`**, **`Date`**, **`Date32`**, **`DateTime64`**, **`UUID`**, **`IPv4`**, **`IPv6`**, **`Enum16`**, **`Bool`** — fixed-width, to be specified in §8.1.

These will be moved into their respective groups (§8.1–8.4) as specifications are filled in.

---

## 9. Compression

> **Placeholder.** This section will document block-level compression. The compression frame format is:
> ```
> [16 bytes: CityHash128 checksum]
> [1 byte: method]         — 0x82=LZ4, 0x90=ZSTD, 0x02=None
> [4 bytes: compressed_size]
> [4 bytes: uncompressed_size]
> [N bytes: compressed_data]
> ```

---

## 10. Packet Type Reference

### 10.1 Client → Server

| Code | Name                      | Body format          | Description |
|------|---------------------------|----------------------|-------------|
| 0    | Hello                     | §7.1                 | Handshake initiation |
| 1    | Query                     | §7.7                 | Query execution request |
| 2    | Data                      | §7.11                | Data block (INSERT data, external tables, end-of-data marker) |
| 3    | Cancel                    | (no body)            | Cancel running query |
| 4    | Ping                      | §7.4 (no body)       | Keepalive check |
| 5    | TablesStatusRequest       | *(not yet specified)* | Table status check |
| 6    | KeepAlive                 | *(not yet specified)* | Connection keepalive |
| 7    | Scalar                    | *(not yet specified)* | Scalar data block |
| 8    | IgnoredPartUUIDs          | *(not yet specified)* | Parts to exclude from query |
| 9    | ReadTaskResponse          | *(not yet specified)* | S3 cluster read response |
| 10   | MergeTreeReadTaskResponse | *(not yet specified)* | Parallel read task response |
| 11   | SSHChallengeRequest       | *(not yet specified)* | SSH auth challenge request |
| 12   | SSHChallengeResponse      | *(not yet specified)* | SSH auth challenge response |
| 13   | QueryPlan                 | *(not yet specified)* | Query plan |

### 10.2 Server → Client

| Code | Name                              | Body format          | Description |
|------|-----------------------------------|----------------------|-------------|
| 0    | Hello                             | §7.2                 | Handshake response |
| 1    | Data                              | §7.11                | Result data block |
| 2    | Exception                         | §7.6                 | Error |
| 3    | Progress                          | §7.12                | Query execution progress |
| 4    | Pong                              | §7.5 (no body)       | Keepalive response |
| 5    | EndOfStream                       | (no body)            | Query complete |
| 6    | ProfileInfo                       | §7.13                | Post-execution profiling data |
| 7    | Totals                            | §7.14                | GROUP BY WITH TOTALS row |
| 8    | Extremes                          | §7.15                | Min/max values (2-row block) |
| 9    | TablesStatusResponse              | *(not yet specified)* | Table status response |
| 10   | Log                               | §7.16                | Query execution log lines |
| 11   | TableColumns                      | §7.18                | Column descriptions for defaults |
| 12   | PartUUIDs                         | *(not yet specified)* | Unique part IDs |
| 13   | ReadTaskRequest                   | *(not yet specified)* | Cluster read task request |
| 14   | ProfileEvents                     | §7.17                | Performance counters |
| 15   | MergeTreeAllRangesAnnouncement    | *(not yet specified)* | Parallel read initialization |
| 16   | MergeTreeReadTaskRequest          | *(not yet specified)* | Parallel read task assignment |
| 17   | TimezoneUpdate                    | *(not yet specified)* | Server timezone update |
| 18   | SSHChallenge                      | *(not yet specified)* | SSH auth challenge |

---

## 11. Implementation Notes

Discoveries and gotchas that aren't obvious from the wire format alone. Each entry documents the symptom, cause, and fix. These are hard-won findings from real implementations; future implementers are strongly encouraged to read through them before debugging misalignment issues.

### 11.1 `ClientInfo.initial_address` must be non-empty in `host:port` format

**Symptom:** Server rejects Query with an assertion violation in `SocketAddress::init()` complaining that `hostAndPort` is empty.

**Cause:** The server parses `initial_address` via a socket address parser that fails if the string is empty.

**Fix:** Always send a valid `host:port` string, e.g., `"127.0.0.1:0"`. Port `0` is fine — the server uses this only for logging, not for actual connections.

---

### 11.2 `ClientInfo.initial_time` is Int64, not VarUInt

**Symptom:** Server rejects Query with `CANNOT_READ_ALL_DATA` — reporting a byte count shortfall (e.g., "Bytes read: 54. Bytes expected: 108.").

**Cause:** `initial_time` is a **fixed-width Int64 (8 bytes, little-endian)** on the wire. Encoding it as VarUInt under-runs the server's expected byte count by up to 7 bytes (VarUInt encodes `0` in 1 byte vs. 8 bytes for Int64).

**Fix:** Use a fixed-width 8-byte little-endian write for `initial_time`. Do not confuse this with other numeric fields in ClientInfo (`version_major`, `version_minor`, `protocol_version`, `distributed_depth`, etc.) which **are** VarUInt.

**General rule:** Within ClientInfo specifically, timestamps are fixed-width; everything else numeric is VarUInt. Always consult the field tables in §7.8 for the authoritative type of each field.

---

### 11.3 `BlockInfo.bucket_number` default is `-1`, not `0`

**Symptom:** Server misinterprets normal result blocks as belonging to aggregation bucket `0`, leading to incorrect distributed query behavior.

**Cause:** `0` is a valid bucket number (first bucket in a two-level GROUP BY aggregation). The "no bucket" sentinel is `-1`.

**Fix:** Default-construct BlockInfo with `bucket_number = -1`. Only set it to a non-negative value when actually emitting bucketed aggregation blocks (inter-server use only; external clients should always send `-1`).

---

### 11.4 Data packets are symmetric — both directions carry `table_name`

**Symptom:** Client hangs on read after query, or decodes garbage for column names/types.

**Cause:** The Data packet wire format is **symmetric** — both directions include an empty `String` (table name) before the Block. Failing to read the `table_name` before decoding the Block on the server → client path misaligns every subsequent field.

**Fix:** When reading a `ServerPacket::Data`, read a `String` first (the table name, almost always empty for query results) before reading the Block body. See §7.11 for the full wire format.

---

### 11.5 Packet type codes are VarUInt, not UInt8

**Symptom:** Works in testing (all current packet type codes are < 128, where VarUInt and UInt8 produce identical bytes), but future packet types ≥ 128 would silently break compatibility.

**Cause:** The protocol's formal encoding for packet type codes is VarUInt, not fixed-width. Current implementations happen to work with UInt8 only because all packet type codes are small (0-18).

**Fix:** Always use VarUInt encoding for packet type codes on both encode and decode paths. See §5.

---

### 11.6 First Data packet after a query is the header block (0 rows)

**Symptom:** `SELECT 1` returns 1 column and **0 rows** instead of 1 row with value `1`.

**Cause:** The server's response to a query is a **stream of packets**, not a single packet:

```
Data (header:  N cols, 0 rows)     ← schema announcement, no data
Data (result:  N cols, M rows)     ← actual data (0 or more such blocks)
...
Data (empty:   0 cols, 0 rows)     ← boundary marker, still NOT the end
EndOfStream                         ← authoritative end of query
```

A client that reads only one Data packet gets the header block — which correctly announces the columns but has zero rows. The actual data arrives in subsequent Data packets.

`num_rows = 0` does **not** mean end-of-query. Only `EndOfStream` (packet type 5) signals the end of a query response.

**Fix:** After sending the Query + end-of-client-data marker, loop reading packets until `EndOfStream` or `Exception`. Treat the first Data packet as the schema; accumulate rows from subsequent Data packets. See §6.4 for the full dispatch table.

---

### 11.7 Column must include `has_custom_serialization` byte at v54454+

**Symptom:** After decoding the first result block, the next `read_response()` call reads what looks like `ServerPacket::Hello` (packet type `0`) — but the handshake is long over. The stream is misaligned by exactly one `0x00` byte per column.

A variant: INSERT data sent with columns is rejected or misparsed by the server, because every column is missing one byte.

**Cause:** At negotiated protocol ≥ 54454 (feature `CUSTOM_SERIALIZATION`), every Column carries a `UInt8` byte after the type string, indicating whether the column uses a non-default serialization (sparse, low-cardinality, etc.). For standard columns, this byte is `0`. See §7.11 Column table, field 3.

Clients that skip this byte read the server's next-packet-type-code out of the middle of the previous packet. Since the byte is `0x00`, it appears to be a `Hello` packet (server packet type 0), but the rest of the stream is garbage.

This pitfall is easy to miss during testing if:
- The client only sends **empty** Data packets (num_columns = 0), so the Column encode path is never exercised, and
- The client only handles the **header** Data packet from the server (which has columns but 0 rows of data, so the misalignment doesn't surface until the next packet is read).

**Fix:** In both `Column::encode` and `Column::decode`, gate reading/writing the `has_custom_serialization` byte on the `CUSTOM_SERIALIZATION` feature:

- Encode: write `0` for standard columns. To represent a non-default serialization, model it explicitly (e.g., a `Serialization` enum with `Default` / `Custom { kind_stack }` variants) and write `1` followed by the kind_stack.
- Decode: read the byte. If `0`, continue. If `1`, either decode the kind_stack or return an `Unsupported` error — whichever matches the client's capability.

Pass the negotiated protocol version through `Block::encode` / `Block::decode` so Column methods can check the feature gate.

---

### 11.8 `Enum8` / `Enum16` are wire-compatible with `Int8` / `Int16`

**Symptom:** Decoding ProfileEvents (or other blocks with Enum columns) fails with "unsupported column type" — even though the spec describes the column as `Int8`.

**Cause:** The server sends types like `Enum8('increment' = 1, 'gauge' = 2)` for columns the spec describes as `Int8` (e.g., the ProfileEvents `type` column, §7.17). The wire bytes are identical to `Int8` — one byte per row — but the type string on the wire differs.

**Fix:** Treat `Enum8` as `Int8` and `Enum16` as `Int16` during column decoding. The preferred approach is to strip the `(...)` parameter suffix from the type string and dispatch on the base name (see §11.9).

---

### 11.9 Column type strings carry parameters — strip before matching

**Symptom:** Decoding a column with type `DateTime('UTC')`, `FixedString(16)`, `Decimal(9, 2)`, `Nullable(UInt32)`, or `Array(Int32)` fails with "unsupported column type" — even when the base type is supported by the client.

**Cause:** Type names on the wire include parameters in parentheses. A decoder that dispatches on the exact type string will miss parameterized variants of supported types. This is pervasive: `DateTime` always carries a timezone, `Decimal` carries precision and scale, and `Enum` / `Nullable` / `Array` / `Tuple` / `Map` all wrap a subtype.

**Fix:** When dispatching on the type string, extract the base type by taking the substring before the first `(`. Example: `"DateTime('UTC')"` and `"DateTime(3)"` both reduce to the base type `"DateTime"`.

The parameter content inside the parentheses may still be needed for decoding (e.g., `Decimal(P, S)` scale affects value interpretation, `FixedString(N)` determines row size, `Nullable(T)` affects wire layout). So don't discard the parameters permanently — just use only the base name for the type dispatch.

---

### 11.10 Unknown column types are a hard decode failure

**Symptom:** Decoding a Data or Log block fails and leaves the stream in an inconsistent state; subsequent packet reads produce garbage.

**Cause:** Unlike fixed-layout packets (Progress, ProfileInfo) where fields have known sizes, column data sizes depend on the type: `UInt32` = 4 bytes per row, `String` = variable (length-prefixed per value), `Array(T)` = offsets + nested element data. Without knowing the type, the decoder cannot compute the byte span of that column to skip over it.

**Fix:** On encountering an unknown column type, the decoder must fail the entire query and terminate or reset the connection. There is no "skip this column" fallback — the stream is permanently misaligned. This motivates supporting at least the common types (UInt and Int variants, String, DateTime, Nullable) before targeting production workloads.

Note the asymmetry with "ignored but still decoded" packets (Log, ProfileEvents): a client may choose to discard the packet's decoded *content* after the fact, but the bytes must still be consumed, and consuming those bytes requires understanding every column type in the block.

---

### 11.11 Log and ProfileEvents must be decoded even when ignored

**Symptom:** Connection hangs or produces garbage after a query that emitted Log or ProfileEvents packets.

**Cause:** The packet envelope (§5) does not include a body length. A client that reads the packet type byte and then attempts to skip to "the next packet" will consume bytes from the middle of the current packet's payload.

**Fix:** Always fully decode the bodies of Log (§7.16) and ProfileEvents (§7.17) packets, even when the client intends to discard the values. The stream position must advance by exactly the body length, and the only way to compute that length is to parse the block structure.

The same reasoning applies to `Totals`, `Extremes`, `TableColumns`, `Progress`, and `ProfileInfo` — a client may ignore the semantic content but must always consume the bytes.

---

### 11.12 Multiple Progress packets are cumulative, not deltas

**Symptom:** Client-side row counts from Progress packets appear inflated (2×, 3×, ... the actual server count).

**Cause:** Each Progress packet carries **cumulative** totals since the start of the query, not deltas from the previous Progress packet. Summing consecutive Progress packets double-counts.

**Fix:** Treat each Progress packet as a snapshot of the query's running totals. Replace the previous value rather than add to it. The last Progress packet received before `EndOfStream` contains the final totals for the query.

---

### 11.13 ProfileEvents `value` column type varies between blocks

**Symptom:** Decoding a ProfileEvents block fails because column 6 (`value`) is declared as `Int64` in one packet and `UInt64` in another.

**Cause:** The `value` column in ProfileEvents (§7.17) is **not** a single fixed type. Each ProfileEvents packet declares its own wire type for the column based on the events it carries: always-increasing counters (e.g., `Query`, `NetworkReceiveBytes`) use `UInt64`, while gauges and delta metrics use `Int64`. The declared column type is uniform within a single packet but may differ between packets during one query's response stream.

**Fix:** Decode the column according to the wire type declared in each packet, not based on an assumed fixed type. Clients that want a unified representation can widen to a signed 64-bit integer, accepting that unsigned values at or above 2^63 either need explicit handling or are treated as a decode error.

A simpler alternative is to store the `value` column as raw bytes plus the type string, deferring interpretation to the caller.

---

### 11.14 Query parameter values must be single-quoted on the wire

**Symptom:** A query like `SELECT {x:UInt32}` with a parameter `x = 42` fails with:
```
DB::Exception: Substitution `x` is not set
```
even though the client sent a parameter named `x`.

**Cause:** Query parameters are transported as custom settings in the Query packet's settings list (§7.9), with the `Custom` flag (`0x02`) set. When the server converts those settings into the query parameter map, it unwraps each value using single-quote-delimited string parsing. A bare value (e.g., `42`) fails this unwrap and the parameter is dropped silently — the server then reports "Substitution is not set" for the named parameter at query-execution time.

**Fix:** Wrap the parameter value in single quotes on encode, and unwrap them on decode. Inner single quotes must be escaped by doubling (`'` → `''`). Examples:

| Logical value | Wire value     |
|---------------|----------------|
| `42`          | `'42'`         |
| `hello`       | `'hello'`      |
| `it's`        | `'it''s'`      |
| empty string  | `''`           |

This quoting is internal to the parameter transport — the query SQL and parameter names are not affected. Only the parameter **value** string needs this treatment.

---

### 11.15 Client must declare a protocol version at or above each feature it needs

**Symptom:** A client feature works against `cargo test` unit tests and against some server versions but silently fails with older-looking behavior — e.g., query parameters appear to be sent but the server doesn't find them; the request succeeds minus the parameter-dependent feature.

**Cause:** The negotiated protocol version is `min(client_declared, server_declared)`. Every feature is gated by a minimum version (§4.3). A client that declares a max version below the feature's gate will not emit that feature on the wire — even if the server supports it.

For example, declaring the client's max version as `Feature::ADDENDUM.version()` (54458) means `Feature::PARAMETERS` (54459) is never active — parameters are silently omitted from the Query packet body because the feature check fails at encode time.

**Fix:** The client's declared `protocol_version` in ClientHello (§7.1) must be at least the maximum version of any feature the client wants to use. In practice, declare the highest version supported by the implementation (i.e., the "Status" line of this spec) and let version negotiation pick the actual working version.

This is a **silent failure mode**: no error is emitted during encoding, and the server often accepts the malformed packet and simply executes the query without the expected feature data. Hard to debug without diffing against known-good packet captures.

---

### 11.16 Tuple parsing requires depth-aware comma splitting; row count comes from inner elements

**Symptom — type parsing:** A `Tuple(Tuple(Int8, Int32), String)` decode fails with a cryptic "unknown type" error, or a `Tuple(Map(String, UInt32))` blows up at the inner Map decode. The decoder believes the element types are fragments like `"Tuple(Int8"` or `"Map(String"` because the type string was split on every comma.

**Cause:** Unlike `Nullable(T)` and `Array(T)`, which have a single inner type that can be extracted with `find('(')` / `rfind(')')`, `Tuple(...)` carries *N* element types separated by `,`. A naive `inner.split(',')` does not know that some commas live inside nested parentheses (other Tuples, Maps, parameterised DateTime, etc.) and splits in the wrong places.

**Fix:** Split with a depth counter. Walk the inner string char by char; track depth (`+1` on `(`, `-1` on `)`); only split when depth `== 0`. Reject the type string if depth doesn't end at `0` (unbalanced parens).

```rust
fn split_with_composite(s: &str) -> Result<Vec<String>> {
    let mut depth = 0i32;
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    if depth != 0 { return Err(/* unbalanced */); }
    out.push(s[start..].trim().to_string());
    Ok(out)
}
```

The same pattern applies the moment any other multi-arg type (`Map(K, V)`, `Variant(T1, T2, ...)`, named tuple element lists) lands in the decoder.

**Symptom — row counting:** Validation passes when it shouldn't, encodes produce malformed streams, or `Array(Tuple(...))` fails at the outer Array's invariant check (`inner.row_count() != offsets.last()`) for a tuple that's actually consistent.

**Cause:** A Tuple's `ColumnData` is most naturally modeled as `Tuple(Vec<ColumnData>)` — a vector of *N* element columns. The temptation is to make `row_count()` return `vec.len()`. But `vec.len()` is the **arity** (number of element types), not the row count. Element columns are *parallel*; the row count is the row count of any one of them.

**Fix:** Implement `row_count()` for Tuple as `vec.first().map_or(0, |c| c.row_count())`. Validate at encode time that all elements agree on row count (and recurse into each to catch nested invariants):

```rust
ColumnData::Tuple(v) => {
    if let Some(first) = v.first() {
        let expected = first.row_count();
        for (i, inner) in v.iter().enumerate() {
            if inner.row_count() != expected {
                return Err(/* element i diverges from element 0 */);
            }
            inner.validate()?;       // recurse — catches nested Array offsets, etc.
        }
    }
    Ok(())
}
```

**Symptom — decoding inner streams:** A multi-row Tuple decodes only the first row's worth of data per element, then either errors out reading past the buffer or produces nonsense for subsequent rows.

**Cause:** Calling `ColumnData::decode(r, element_type, 1)` for each element treats the wire as one value per element rather than `num_rows` values. Tuple's wire format is per-element streams of `num_rows` values each (§8.3.3); the outer `num_rows` must be passed through unchanged.

**Fix:** `ColumnData::decode(r, element_type, rows)` — same `rows` for every element.

---

## 12. Configuration

This section documents the tunables that shape native protocol connections. Two categories:

- **§12.1 Transport-layer settings** — TCP socket options and timeouts. Affect how the TCP connection itself behaves (latency, failure detection, connection lifetime).
- **§12.2 Application-layer settings** — per-query tunables the client may include in the Query packet's `settings` list (§7.9). Affect what the server sends on the wire or how it's framed.

All defaults below reflect the behavior of the reference server implementation at the protocol versions covered by this document. Values may differ across server versions and deployments.

### 12.1 Transport-Layer Settings

These settings affect the TCP socket and the connection's physical transport. They are negotiated or applied at handshake time and typically do not change mid-connection.

#### 12.1.1 Socket options

| Option | Default | Unit | Side | Description |
|--------|---------|------|------|-------------|
| `TCP_NODELAY` | on | bool | both | Nagle's algorithm disabled on both ends. Small packets (handshake, Ping, short Query packets) are sent immediately without coalescing delay. |
| `SO_KEEPALIVE` | on (client), OS default (server) | bool | asymmetric | Kernel-level TCP keepalive probes. **Client** explicitly enables the socket option when `tcp_keep_alive_timeout > 0`. **Server** does not explicitly set this option on the native protocol socket; it inherits the OS default (typically off, or with a long idle interval like 2 hours). This asymmetry means the **client** detects silently broken connections faster than the server. |
| `SO_RCVBUF` / `SO_SNDBUF` | not set (OS defaults) | bytes | — | Socket receive/send buffer sizes. Not tuned by the protocol; OS defaults apply. Large result streaming may benefit from increased buffer sizes at the OS level. |

#### 12.1.2 Timeouts

| Setting | Default | Unit | Side | Description |
|---------|---------|------|------|-------------|
| `connect_timeout` | 10 | seconds | client | Timeout for establishing the initial TCP connection. Exceeding this aborts the connection attempt. |
| `handshake_timeout_ms` | 10000 | milliseconds | client | Timeout for receiving ServerHello during handshake. Applied as both send and receive timeout during the handshake phase. |
| `send_timeout` | 300 | seconds | both | If no bytes can be written to the socket within this interval, the connection throws. Bidirectional: client's `send_timeout` becomes server's `receive_timeout` expectation and vice versa. |
| `receive_timeout` | 300 | seconds | both | If no bytes can be read from the socket within this interval, the connection throws. Bidirectional (see above). |
| `tcp_keep_alive_timeout` | 290 | seconds | client | Idle duration before the client's OS sends the first TCP keepalive probe. Deliberately less than `receive_timeout` so the kernel detects dead servers before the application's receive timeout fires. Applied via `TCP_KEEPIDLE` on Linux / `TCP_KEEPALIVE` on macOS. Server-side keepalive is not set by the native protocol path and falls back to OS defaults; this asymmetry is intentional — the server relies on `idle_connection_timeout` (§12.1.3) for its own dead-peer detection. |
| `receive_data_timeout_ms` | 2000 | milliseconds | client | Timeout for receiving the first Data packet (or Progress packet indicating work) from a replica. Used in multi-replica (failover / hedged) connections. |
| `connect_timeout_with_failover_ms` | 1000 | milliseconds | client | Per-attempt connect timeout when iterating replicas (non-TLS). Shorter than base `connect_timeout` because failing fast allows faster failover. |
| `connect_timeout_with_failover_secure_ms` | 1000 | milliseconds | client | Per-attempt connect timeout when iterating replicas over TLS. |
| `hedged_connection_timeout_ms` | 50 | milliseconds | client | Per-attempt connect timeout for hedged (speculative parallel) requests. Very short — hedging only makes sense when attempts are cheap. |
| `poll_interval` | 10 | seconds | server | Granularity of the server's idle-connection and shutdown check loop. Not a user-visible latency; affects how quickly the server detects shutdown signals and idle-connection expiry. |

**Timing relationship to be aware of:**

```
tcp_keep_alive_timeout (290s)
      < receive_timeout (300s)
      < idle_connection_timeout (3600s)
      < tcp_close_connection_after_queries_seconds (0 = unlimited by default)
```

OS keepalive fires first and may detect dead peers silently (at the kernel level). Application receive timeout is the next line of defense. Idle timeout is the last resort that reaps long-unused connections.

#### 12.1.3 Connection limits

| Setting | Default | Unit | Side | Description |
|---------|---------|------|------|-------------|
| `max_connections` | 4096 | count | server | Maximum concurrent TCP connections the server accepts. Additional connections are refused at the TCP layer. |
| `idle_connection_timeout` | 3600 | seconds | server | Maximum time an **idle** (no active query, no in-flight request) connection may remain open. Server closes connections exceeding this threshold during the poll loop. |
| `tcp_close_connection_after_queries_num` | 0 (unlimited) | count | server | Maximum number of queries per connection before the server forcibly closes it. Useful for forced connection recycling during rolling deploys. |
| `tcp_close_connection_after_queries_seconds` | 0 (unlimited) | seconds | server | Maximum **total** connection lifetime in seconds regardless of activity. Server closes any connection older than this threshold. Useful for ensuring connections eventually re-authenticate and re-balance. |

**Long-lived connections by default.** A connection that issues queries regularly (even infrequently) can live indefinitely. Only idle connections are reaped after 1 hour. There is **no default maximum lifetime** — production deployments wishing to force periodic reconnects must set `tcp_close_connection_after_queries_seconds` explicitly.

### 12.2 Application-Layer Settings

These settings are carried per-query in the Query packet's `settings` list (§7.7, §7.9). They change **what the server sends on the wire or how it's framed**, distinct from settings that affect SQL execution semantics (`max_threads`, `max_memory_usage`, etc.), which are not protocol concerns.

#### 12.2.1 Compression

| Setting | Default | Unit | Description |
|---------|---------|------|-------------|
| `network_compression_method` | `"LZ4"` | string | Compression codec applied to data blocks on the wire when the Query packet's `compression` flag is set. Values: `"LZ4"`, `"LZ4HC"`, `"ZSTD"`, `"NONE"`. Affects the compression method byte in the compressed frame (§9). |
| `network_zstd_compression_level` | 1 | 1–15 | ZSTD compression level when `network_compression_method == "ZSTD"`. Higher values produce smaller compressed blocks at higher CPU cost. No effect for other codecs. |

The `compression` flag in the Query packet itself (§7.7 field 6) toggles compression on/off for that query. These settings select **which** codec is used when it's on.

#### 12.2.2 Log streaming

| Setting | Default | Unit | Description |
|---------|---------|------|-------------|
| `send_logs_level` | `"fatal"` | string | Minimum log level to include in Log packets (§7.16). Values: `"none"`, `"fatal"`, `"error"`, `"warning"`, `"information"`, `"debug"`, `"trace"`. Setting to `"none"` suppresses Log packets entirely. |
| `send_logs_source_regexp` | `""` (all sources) | string | Regex filter on the logger name column of Log packets. Only log lines whose source matches are transmitted. Empty = all sources pass. |

Setting `send_logs_level` to anything other than `"none"` causes the server to emit `Log` packets during query execution (§7.16). Clients that don't care about logs should leave the default (or set `"none"`) to reduce wire volume.

#### 12.2.3 Progress reporting

| Setting | Default | Unit | Description |
|---------|---------|------|-------------|
| `interactive_delay` | 100000 | microseconds | Target minimum interval between consecutive Progress packets (§7.12) from server to client. Smaller values produce more Progress packets (more wire traffic, finer-grained UI updates). Also affects how often the server checks for client cancellation. |

Note: `interactive_delay` is the **target minimum** between Progress packets. The server may send Progress packets less frequently if the query is not producing work fast enough to trigger them.

#### 12.2.4 Result envelope

| Setting | Default | Unit | Description |
|---------|---------|------|-------------|
| `extremes` | false | bool | When true, the server sends an additional `Extremes` packet (§7.15) carrying min/max values per column after the result data. When false, no Extremes packet is emitted. |
| `max_result_rows` | 0 (unlimited) | count | Cap on the number of rows the server transmits. When exceeded, behavior is controlled by `result_overflow_mode`. |
| `max_result_bytes` | 0 (unlimited) | uncompressed bytes | Cap on the uncompressed byte volume the server transmits. Behavior on overflow follows `result_overflow_mode`. |
| `result_overflow_mode` | `"throw"` | string | Behavior when a result cap is exceeded: `"throw"` ends the stream with an Exception packet (§7.6); `"break"` sends partial results followed by a normal `EndOfStream` (§10.2). |

#### 12.2.5 Async INSERT (relevant for INSERT queries only)

| Setting | Default | Unit | Description |
|---------|---------|------|-------------|
| `async_insert` | true | bool | When true, INSERT data is queued server-side and batched with other inserts instead of flushing immediately. Affects the timing of the server's response (EndOfStream or Exception) but does not change the wire packet types. |
| `wait_for_async_insert` | true | bool | When true (with `async_insert` on), the server holds the response until the queued data is flushed. When false, the server returns immediately after queuing, before the data is actually durable. |
| `wait_for_async_insert_timeout` | 120 | seconds | Maximum time the server waits for flush before returning (when `wait_for_async_insert` is true). Past this, the server returns success even if flush hasn't completed. |

#### 12.2.6 Distributed tracing

| Setting | Default | Unit | Description |
|---------|---------|------|-------------|
| `opentelemetry_start_trace_probability` | 0.0 | 0–1 probability | Server-side probability of attaching OpenTelemetry trace context to response-side telemetry. Does not directly affect client → server wire format (which is governed by the client's own ClientInfo OpenTelemetry fields, §7.8 field 16). |

---

### 12.3 Settings Explicitly Out of Scope

These are commonly confused with protocol-level settings but actually control SQL execution, storage, or CPU use — not wire behavior. A protocol implementation does not need to handle them specially:

- `max_threads` — parallelism within query execution
- `max_memory_usage` — per-query memory cap
- `max_block_size`, `preferred_block_size_bytes` — internal block sizing during query processing; the blocks transmitted on the wire may be of any size independent of these
- `compile_expressions` — JIT compilation of expressions; CPU-only
- `async_insert_max_data_size` — server-side queue buffer; not a wire concern
- All settings prefixed `input_format_*` and `output_format_*` — apply to non-native formats (CSV, TSV, JSONEachRow, etc.) over the HTTP interface, not the native protocol

### 12.4 Settings Not Yet Covered

The chunked protocol (negotiated via the addendum at v54470+) introduces additional transport tunables that are not yet documented in this spec:

- `proto_send_chunked`, `proto_recv_chunked` — negotiated mode (`chunked`, `notchunked`, `chunked_optional`, `notchunked_optional`)
- Chunk framing, length prefixes, chunk-level flow control

These will be added in a future revision.
