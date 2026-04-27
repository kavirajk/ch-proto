# ClickHouse Native Protocol & Native Format Specification

This document has been split into three companion files:

- **[`NATIVE_PROTOCOL.md`](NATIVE_PROTOCOL.md)** — the TCP wire protocol: packet framing, connection state machine, message bodies, configuration, packet type reference.
- **[`NATIVE_FORMAT.md`](NATIVE_FORMAT.md)** — the columnar data format: wire primitives, Block/Column structure, data type encodings, compression frame.
- **[`IMPLEMENTATION_NOTES.md`](IMPLEMENTATION_NOTES.md)** — gotchas, footguns, and reference-implementation status. Symptom/Cause/Fix entries collected from real implementations.

The two spec documents are language- and implementation-neutral. Implementation-specific commentary (Rust crates, library choices, file paths, project status) lives only in `IMPLEMENTATION_NOTES.md`.

A reader new to the protocol should read `NATIVE_PROTOCOL.md` and `NATIVE_FORMAT.md` in either order — they are independent specs that cross-reference where needed. The `IMPLEMENTATION_NOTES.md` is a debug-pointer document, not normative spec; consult it when something on the wire isn't behaving as the spec describes.
