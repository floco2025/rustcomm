use rustcomm::prelude::*;
use std::collections::HashMap;
use std::net::{SocketAddr, Shutdown};
use std::thread;


#[test]
fn transport_mesh_tcp() {
    println!("=== P2P Mesh TCP Transport Test ===\n");
    let config = config::Config::builder()
        .set_default("transport_type", "tcp")
        .unwrap()
        .build()
        .unwrap();
    run_transport_mesh_test(config);
}

#[test]
fn transport_mesh_tls() {
    println!("=== P2P Mesh TLS Transport Test ===\n");

    let config = config::Config::builder()
        .set_default("transport_type", "tls")
        .unwrap()
        .set_default("tls_server_cert", "tests/cert.pem")
        .unwrap()
        .set_default("tls_server_key", "tests/key.pem")
        .unwrap()
        .set_default("tls_ca_cert", "tests/cert.pem")
        .unwrap()
        .build()
        .unwrap();
    run_transport_mesh_test(config);
}

#[cfg(feature = "quic")]
#[test]
fn transport_mesh_quic() {
    println!("=== P2P Mesh QUIC Transport Test ===\n");

    let config = config::Config::builder()
        .set_default("transport_type", "quic")
        .unwrap()
        .set_default("tls_server_cert", "tests/cert.pem")
        .unwrap()
        .set_default("tls_server_key", "tests/key.pem")
        .unwrap()
        .set_default("tls_ca_cert", "tests/cert.pem")
        .unwrap()
        .build()
        .unwrap();
    run_transport_mesh_test(config);
}

fn run_transport_mesh_test(config: config::Config) {
    println!("Starting 3 peers in a fully connected mesh...\n");

    let mut transport_a = Transport::new(&config).expect("Failed to create transport A");
    transport_a.listen("127.0.0.1:0").expect("Failed to listen");
    transport_a.listen("127.0.0.1:0").expect("Failed to listen");
    let addrs_a = transport_a.get_listener_addresses();
    println!("[A] Listening on {:?}", addrs_a);

    let mut transport_b = Transport::new(&config).expect("Failed to create transport B");
    transport_b.listen("127.0.0.1:0").expect("Failed to listen");
    transport_b.listen("127.0.0.1:0").expect("Failed to listen");
    let addrs_b = transport_b.get_listener_addresses();
    println!("[B] Listening on {:?}", addrs_b);

    let mut transport_c = Transport::new(&config).expect("Failed to create transport C");
    transport_c.listen("127.0.0.1:0").expect("Failed to listen");
    transport_c.listen("127.0.0.1:0").expect("Failed to listen");
    let addrs_c = transport_c.get_listener_addresses();
    println!("[C] Listening on {:?}", addrs_c);

    // Determine connection targets for each peer
    // A connects to B[0] and C[0]
    let connect_a = vec![addrs_b[0], addrs_c[0]];
    // B connects to A[0] and C[1]
    let connect_b = vec![addrs_a[0], addrs_c[1]];
    // C connects to A[1] and B[1]
    let connect_c = vec![addrs_a[1], addrs_b[1]];

    // Spawn peer threads with their transports
    let peer_a = thread::spawn(move || run_peer("A", transport_a, connect_a));
    let peer_b = thread::spawn(move || run_peer("B", transport_b, connect_b));
    let peer_c = thread::spawn(move || run_peer("C", transport_c, connect_c));

    // Wait for all peers to complete
    peer_a.join().expect("Peer A failed");
    peer_b.join().expect("Peer B failed");
    peer_c.join().expect("Peer C failed");

    println!("\n=== All peers completed successfully! ===");
}

fn run_peer(name: &str, mut transport: Transport, connect_addrs: Vec<SocketAddr>) {
    println!("[{name}] Starting peer...");

    // Connect to other peers and track outgoing connection IDs
    let mut outgoing_connections = Vec::new();
    for addr in &connect_addrs {
        let (conn_id, _peer_addr) = transport.connect(addr).expect("Failed to connect");
        println!("[{name}] Connecting to {addr} (conn_id: {conn_id})");
        outgoing_connections.push(conn_id);
    }

    // Track connected peers and received data
    let mut connected_peers: HashMap<usize, String> = HashMap::new();
    let mut received_data: HashMap<usize, Vec<u8>> = HashMap::new();
    let expected_connections = 4; // Each peer has 2 outgoing + 2 incoming = 4 total connections
    let expected_data_size = 1024;

    // Main event loop
    loop {
        let events = transport.fetch_events().expect("Failed to fetch events");

        for event in events {
            match event {
                TransportEvent::Connected { id } => {
                    println!("[{name}] Connection established (id: {id})");
                    connected_peers.insert(id, format!("Peer-{id}"));

                    // Once all connections are established, send data
                    if connected_peers.len() == expected_connections {
                        // Send data over all connections (both outgoing and incoming)
                        for &peer_id in connected_peers.keys() {
                            let data = vec![42u8; expected_data_size];
                            transport.send_to(peer_id, data);
                            println!(
                                "[{name}] Sent {expected_data_size} bytes to peer (id: {peer_id})"
                            );
                        }
                    }
                }
                TransportEvent::ConnectionFailed { id } => {
                    panic!("[{name}] Connection failed unexpectedly (id: {id})");
                }
                TransportEvent::Disconnected { id } => {
                    println!("[{name}] Peer disconnected (id: {id})");
                    connected_peers.remove(&id);
                    if connected_peers.is_empty() {
                        return;
                    }
                }
                TransportEvent::Data { id, data } => {
                    // Accumulate data from this connection
                    received_data
                        .entry(id)
                        .or_insert_with(Vec::new)
                        .extend_from_slice(&data);

                    println!("[{name}] Received {} bytes from id {id}", data.len());

                    // Check if we've received complete data from all connections
                    if received_data.len() == expected_connections {
                        let all_received = received_data
                            .values()
                            .all(|v| v.len() == expected_data_size);

                        if all_received {
                            println!(
                                "[{name}] All data received! Received {expected_data_size} bytes from {expected_connections} connections"
                            );

                            transport.shutdown_all_connections(Shutdown::Read);
                        }
                    }
                }
                _ => {
                    panic!("Unexpected event");
                }
            }
        }
    }
}
