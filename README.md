# RustComm

A lightweight, high-performance communication library built on
[mio](https://docs.rs/mio) with TCP and TLS transport support.

- **Dual-layer architecture:** Low-level `Transport` API for raw bytes, or
  higher-level `Messenger` API for type-safe serialized messages
- **Peer-to-peer:** No fixed server/client roles - any peer can listen, connect,
  send, and receive
- **Serialization-agnostic:** Bring your own serialization (bincode, serde,
  hand-written, etc.) - includes optional bincode helpers
- **Easily extensible:** Add custom transports beyond TCP/TLS, such as QUIC.
- **Flexible threading:** Works in single-threaded event loops or multi-threaded
  architectures like thread pools

## Documentation

See the full documentation on [docs.rs/rustcomm](https://docs.rs/rustcomm) for:
- Quick start examples
- Configuration guide
- API reference

## Examples

The repository includes several examples:

- **minimal** - Simplest messenger-based client-server
- **minimal_transport** - Simplest transport-based (raw bytes) client-server
- **p2p_mesh** - Peer-to-peer mesh network demonstration
- **thread_pool** - Multi-threaded message processing with worker pool
- **chat** - Full-featured chat application with multiple server variants

Run an example:

```bash
cargo run --example minimal
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
