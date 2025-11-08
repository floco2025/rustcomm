# RustComm Examples

Examples demonstrating various features and use cases of the RustComm library.

## Basic Examples

### minimal.rs
The simplest messenger-based client-server example. Server echoes back typed messages.
```bash
cargo run --example minimal
```

### minimal_transport.rs
The simplest transport-based example using raw bytes instead of typed messages.
```bash
cargo run --example minimal_transport
```

## Advanced Examples

### p2p_mesh.rs
Peer-to-peer mesh network where 3 peers form a fully connected topology. Each
peer both listens for connections and connects to others, demonstrating
bidirectional peer-to-peer communication.
```bash
cargo run --example p2p_mesh
```

### thread_pool.rs
Multi-threaded server using a thread pool to process requests in parallel.
Demonstrates graceful shutdown using `close_all()` and the `Inactive` event.
```bash
cargo run --example thread_pool
```

### custom_serialization.rs
Demonstrates using different serialization formats (bincode, JSON, MessagePack)
within the same application. Shows both the convenient macro-based registration
and manual registration with custom serializers.
```bash
cargo run --example custom_serialization
```

## Chat Application

Full-featured chat application with multiple server variants and TLS support.

### chat/client.rs
Interactive chat client with room support, private messages, and user
management.
```bash
cargo run --bin chat_client -- <server_address>
```

### chat/server_single.rs
Single-threaded chat server handling all clients in one event loop.
```bash
cargo run --bin chat_server_single -- --bind 127.0.0.1:8080
```

### chat/server_multi.rs
Multi-threaded chat server with worker pool for concurrent message processing.
```bash
cargo run --bin chat_server_multi -- --bind 127.0.0.1:8080
```

### TLS Support
All chat examples support TLS via configuration files:
```bash
cargo run --bin chat_server_single -- --config chat-tls.toml
```

See `chat/chat-tls.toml` for example TLS configuration.
