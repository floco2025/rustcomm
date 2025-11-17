use rustcomm::transport::{Transport, TransportEvent};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

#[test]
fn tcp_reports_no_multi_stream_support() {
    let config = config::Config::builder()
        .set_default("transport_type", "tcp")
        .unwrap()
        .build()
        .unwrap();

    let transport = Transport::new(&config).expect("Failed to create TCP transport");
    assert!(!transport.supports_multi_stream());
}

#[cfg(feature = "quic")]
#[test]
fn quic_multi_stream_roundtrip() {
    let _ = tracing_subscriber::fmt::try_init();

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

    // Server setup and loop
    let mut server = Transport::new(&config).expect("Failed to create server transport");
    server
        .listen("127.0.0.1:0")
        .expect("Server failed to listen");
    let server_addr = server.get_listener_addresses()[0];
    assert!(server.supports_multi_stream());
    let server_interface = server.get_transport_interface();
    let (server_events, server_stop, server_handle) = spawn_transport_thread(server);

    // Client setup and loop
    let client = Transport::new(&config).expect("Failed to create client transport");
    assert!(client.supports_multi_stream());
    let client_interface = client.get_transport_interface();
    let (client_events, client_stop, client_handle) = spawn_transport_thread(client);

    let (client_conn_id, _) = client_interface
        .connect(server_addr)
        .expect("Client failed to connect");

    // Wait for both transports to report the base connection.
    let server_conn_id = wait_for_connected(&server_events, None);
    let _client_conn_event = wait_for_connected(&client_events, Some(client_conn_id));

    // Open secondary stream from the client and wait until both sides see it.
    let client_stream_id = client_interface
        .open_stream(client_conn_id)
        .expect("Failed to open client stream");
    wait_for_connected(&client_events, Some(client_stream_id));

    // Send payload across the new stream and ensure the server receives both
    // the virtual connection announcement and the bytes themselves.
    let payload = b"hello multi-stream".to_vec();
    client_interface.send_to(client_stream_id, payload.clone());
    let server_stream_id = wait_for_stream_payload(&server_events, server_conn_id, &payload);
    assert_ne!(server_stream_id, server_conn_id);

    // Tear down threads cleanly.
    client_interface.close_all();
    server_interface.close_all();
    let _ = client_stop.send(());
    let _ = server_stop.send(());
    let _ = client_handle.join();
    let _ = server_handle.join();
}

#[cfg(feature = "quic")]
fn spawn_transport_thread(
    mut transport: Transport,
) -> (
    Receiver<TransportEvent>,
    mpsc::Sender<()>,
    thread::JoinHandle<()>,
) {
    let (event_tx, event_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();

    let handle = thread::spawn(move || loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        match transport.fetch_events() {
            Ok(events) => {
                for event in events {
                    if event_tx.send(event).is_err() {
                        return;
                    }
                }
            }
            Err(err) => panic!("transport loop error: {err:?}"),
        }
    });

    (event_rx, stop_tx, handle)
}

#[cfg(feature = "quic")]
fn wait_for_connected(events: &Receiver<TransportEvent>, expected: Option<usize>) -> usize {
    loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(TransportEvent::Connected { id }) => {
                if expected.is_none() || expected == Some(id) {
                    return id;
                }
            }
            Ok(TransportEvent::Inactive) => {
                continue;
            }
            Ok(_) => {}
            Err(err) => panic!("Timed out waiting for Connected event: {err}"),
        }
    }
}

#[cfg(feature = "quic")]
fn wait_for_stream_payload(
    events: &Receiver<TransportEvent>,
    base_conn_id: usize,
    payload: &[u8],
) -> usize {
    let mut announced_stream_id: Option<usize> = None;
    let mut buffered_data: Option<(usize, Vec<u8>)> = None;

    loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(TransportEvent::Connected { id }) => {
                if id != base_conn_id {
                    println!("server event: Connected {{{}}}", id);
                    if let Some((pending_id, data)) = buffered_data.take() {
                        if pending_id == id {
                            assert_eq!(data, payload);
                            return id;
                        } else {
                            buffered_data = Some((pending_id, data));
                        }
                    }
                    announced_stream_id = Some(id);
                }
            }
            Ok(TransportEvent::Data { id, data }) => {
                if id == base_conn_id {
                    continue;
                }
                println!("server event: Data {{{}}} bytes={} ", id, data.len());
                if Some(id) == announced_stream_id {
                    assert_eq!(data, payload);
                    return id;
                } else {
                    buffered_data = Some((id, data));
                }
            }
            Ok(TransportEvent::Inactive) => continue,
            Ok(_) => {}
            Err(err) => panic!("Timed out waiting for stream data: {err}"),
        }
    }
}
