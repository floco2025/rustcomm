# RustComm

A lightweight, high-performance communication library built on
[mio](https://docs.rs/mio) with built-in TCP, TLS, and QUIC transport support.

- **Three-layer architecture:**
  - **Transport:** Low-level API for raw bytes over TCP/TLS/QUIC
  - **Messenger:** Type-safe serialized messages with any serialization format
  - **RpcMessenger:** Async RPC with request-response correlation (optional `rpc` feature)
- **Peer-to-peer:** No fixed server/client roles - any peer can listen, connect,
  send, and receive
- **Easily extensible:** Ships with TCP/TLS/QUIC and stays extensible for additional transports
- **Flexible threading:** Works in single-threaded event loops or multi-threaded
  architectures like thread pools

**Optional features (enabled by default):** `tls`, `quic`, `rpc`, and `bincode`.
Disable them in `Cargo.toml` if you need a smaller footprint.

## Architecture

RustComm provides three layers you can choose from based on your needs:

### 1. Transport Layer (Raw Bytes)
The lowest level - send and receive raw bytes over TCP, TLS, or QUIC
connections.
- Direct control over data format
- Minimal overhead
- You handle framing and serialization

### 2. Messenger Layer (Typed Messages)
Built on Transport - automatic framing and type-safe message dispatch.
- Messages are Rust types implementing the `Message` trait
- Message registry for handling different message types
- Automatic serialization/deserialization
- Bring your own serialization (bincode, serde, hand-written, etc.) - includes
  optional bincode helpers

### 3. RPC Layer (Async Request-Response)
⚠️ **WORK IN PROGRESS - DO NOT USE** ⚠️

Built on Messenger - async request-response pattern for RPC-style communication.
- Async/await API with `send_request()` that returns typed responses
- Works with tokio, async-std, smol, or even no runtime at all with `futures::executor`
- Automatic request/response matching by ID
- Optional feature: enable with `rpc` (enabled by default)

**This API is unstable and will change.** Do not use this yet until it is finished.

**Choose your layer:**
- Need raw bytes? Use **Transport**
- Need typed messages? Use **Messenger**  
- Need async RPC? Use **RpcMessenger**

## Documentation

See the full documentation on [docs.rs/rustcomm](https://docs.rs/rustcomm) for:
- Quick start examples
- Configuration guide
- API reference

## Examples

The repository includes several examples demonstrating each layer:

**Transport Layer (Raw Bytes):**
- **minimal_transport** - Simplest transport-based client-server

**Messenger Layer (Typed Messages):**
- **minimal** - Simplest messenger-based client-server
- **p2p_mesh** - Peer-to-peer mesh network demonstration
- **thread_pool** - Multi-threaded message processing with worker pool
- **custom_serialization** - Using custom serialization instead of bincode
- **chat** - Full-featured chat application with multiple server variants

**RpcMessenger Layer:**
- **async** - Request-response pattern with runtime-agnostic async

Run an example:

```bash
cargo run --example minimal
cargo run --example async
cargo run --example chat_server_single
cargo run --example chat_client
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

## Contribution

Any contribution intentionally submitted for inclusion in the work by you shall
be dual licensed as MIT OR Apache-2.0, without any additional terms or
conditions.
