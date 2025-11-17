#![cfg(feature = "quic")]

use rustcomm::prelude::*;
use rustcomm::Error;
use std::collections::{HashMap, HashSet};
use std::net::{Shutdown, SocketAddr};
use std::thread;

const STREAMS_PER_CONNECTION: usize = 2;
const STREAM_PAYLOAD_SIZE: usize = 512;

#[test]
fn transport_mesh_quic_multistream() {
    println!("=== P2P Mesh QUIC Multi-stream Transport Test ===\n");

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
    println!("Starting 3 peers with multi-stream connections...\n");

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

    let connect_a = vec![addrs_b[0], addrs_c[0]];
    let connect_b = vec![addrs_a[0], addrs_c[1]];
    let connect_c = vec![addrs_a[1], addrs_b[1]];

    let peer_a = thread::spawn(move || run_peer("A", transport_a, connect_a));
    let peer_b = thread::spawn(move || run_peer("B", transport_b, connect_b));
    let peer_c = thread::spawn(move || run_peer("C", transport_c, connect_c));

    peer_a.join().expect("Peer A failed");
    peer_b.join().expect("Peer B failed");
    peer_c.join().expect("Peer C failed");

    println!("\n=== All peers completed multi-stream exchange successfully! ===");
}

fn run_peer(name: &str, mut transport: Transport, connect_addrs: Vec<SocketAddr>) {
    println!("[{name}] Starting peer...");
    assert!(transport.supports_multi_stream());

    for addr in &connect_addrs {
        let (conn_id, _peer_addr) = transport.connect(addr).expect("Failed to connect");
        println!("[{name}] Initiated connection {conn_id} -> {addr}");
    }

    let expected_connections = 4;
    let expected_remote_streams = expected_connections * STREAMS_PER_CONNECTION;

    let mut base_connections: HashSet<usize> = HashSet::new();
    let mut streams_opened_for: HashSet<usize> = HashSet::new();
    let mut local_stream_ids: HashSet<usize> = HashSet::new();
    let mut remote_stream_progress: HashMap<usize, usize> = HashMap::new();
    let mut remote_streams_completed = 0;

    loop {
        let events = transport
            .fetch_events()
            .expect("Failed to fetch transport events");

        for event in events {
            match event {
                TransportEvent::Connected { id } => {
                    if streams_opened_for.contains(&id) {
                        continue;
                    }

                    match open_peer_streams(name, &mut transport, id, &mut local_stream_ids) {
                        Ok(()) => {
                            println!("[{name}] Base connection established (id: {id})");
                            streams_opened_for.insert(id);
                            base_connections.insert(id);
                        }
                        Err(Error::ConnectionNotFound { .. }) => {
                            println!("[{name}] Remote stream announced (id: {id})");
                        }
                        Err(err) => panic!("[{name}] Failed to open streams: {err:?}"),
                    }
                }
                TransportEvent::Data { id, data } => {
                    if base_connections.contains(&id) || local_stream_ids.contains(&id) {
                        continue;
                    }

                    let entry = remote_stream_progress.entry(id).or_insert(0);
                    *entry += data.len();
                    if *entry >= STREAM_PAYLOAD_SIZE {
                        remote_stream_progress.remove(&id);
                        remote_streams_completed += 1;
                        println!(
                            "[{name}] Completed remote stream {id} ({rem}/{exp})",
                            rem = remote_streams_completed,
                            exp = expected_remote_streams
                        );
                        if remote_streams_completed == expected_remote_streams {
                            println!("[{name}] Received all remote stream payloads");
                            transport.shutdown_all_connections(Shutdown::Read);
                        }
                    }
                }
                TransportEvent::Disconnected { id } => {
                    if base_connections.remove(&id) {
                        println!("[{name}] Base connection closed (id: {id})");
                        if base_connections.is_empty() {
                            println!("[{name}] All base connections closed. Peer done.");
                            return;
                        }
                    }
                }
                TransportEvent::ConnectionFailed { id } => {
                    panic!("[{name}] Connection failed unexpectedly (id: {id})");
                }
                TransportEvent::Inactive => continue,
            }
        }
    }
}

fn open_peer_streams(
    name: &str,
    transport: &mut Transport,
    conn_id: usize,
    local_stream_ids: &mut HashSet<usize>,
) -> Result<(), Error> {
    for stream_idx in 0..STREAMS_PER_CONNECTION {
        let stream_id = transport.open_stream(conn_id)?;
        local_stream_ids.insert(stream_id);
        let fill_byte = name.as_bytes()[0];
        let mut payload = vec![fill_byte; STREAM_PAYLOAD_SIZE];
        payload[stream_idx] = payload[stream_idx].wrapping_add((stream_idx as u8) + 1);
        transport.send_to(stream_id, payload);
        println!(
            "[{name}] Opened stream {stream_id} on connection {conn_id} (stream #{stream_idx})"
        );
    }
    Ok(())
}
