/// Integration tests that run non-interactive examples
///
/// These tests verify that examples compile and execute successfully
/// without hanging or panicking.
use std::process::{Command, Stdio};

fn run_example(name: &str) {
    let status = Command::new("cargo")
        .args(&["run", "--example", name, "--quiet"])
        .stdout(Stdio::null()) // Suppress stdout
        .stderr(Stdio::null()) // Suppress stderr
        .status()
        .unwrap_or_else(|e| panic!("Failed to run example {}: {}", name, e));

    assert!(
        status.success(),
        "Example '{}' failed with exit code: {:?}",
        name,
        status.code()
    );
}

#[test]
fn test_minimal_example() {
    run_example("minimal");
}

#[test]
fn test_minimal_transport_example() {
    run_example("minimal_transport");
}

#[test]
fn test_p2p_mesh_example() {
    run_example("p2p_mesh");
}

#[test]
fn test_thread_pool_example() {
    run_example("thread_pool");
}

#[test]
fn test_custom_serialization_example() {
    run_example("custom_serialization");
}
