use std::io::Write;
use tempfile::NamedTempFile;

/// Guard that holds temporary certificate files and auto-cleans them on drop
pub struct TlsCertGuard {
    _cert_file: NamedTempFile,
    _key_file: NamedTempFile,
    _ca_cert_file: NamedTempFile,
}

/// Generate TLS config for testing with both server and client settings
/// Returns (config, cleanup_guard)
pub fn generate_test_tls_config_separate() -> (config::Config, TlsCertGuard) {
    let (cert_file, key_file, ca_cert_file) = create_temp_cert_files();

    let config = config::Config::builder()
        .set_default("tls_server_cert", cert_file.path().to_str().unwrap())
        .unwrap()
        .set_default("tls_server_key", key_file.path().to_str().unwrap())
        .unwrap()
        .set_default("tls_ca_cert", ca_cert_file.path().to_str().unwrap())
        .unwrap()
        .build()
        .unwrap();

    (
        config,
        TlsCertGuard {
            _cert_file: cert_file,
            _key_file: key_file,
            _ca_cert_file: ca_cert_file,
        },
    )
}

/// Create temporary certificate files with self-signed cert
fn create_temp_cert_files() -> (NamedTempFile, NamedTempFile, NamedTempFile) {
    // Create shared certificate for testing
    let certified_key = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_pem = certified_key.cert.pem();
    let key_pem = certified_key.key_pair.serialize_pem();

    // Create temporary files that will auto-delete on drop
    let mut cert_file = NamedTempFile::new().unwrap();
    let mut key_file = NamedTempFile::new().unwrap();
    let mut ca_cert_file = NamedTempFile::new().unwrap();

    cert_file.write_all(cert_pem.as_bytes()).unwrap();
    key_file.write_all(key_pem.as_bytes()).unwrap();
    // For testing, CA cert is the same as server cert (self-signed)
    ca_cert_file.write_all(cert_pem.as_bytes()).unwrap();

    // Flush to ensure files are written before use
    cert_file.flush().unwrap();
    key_file.flush().unwrap();
    ca_cert_file.flush().unwrap();

    (cert_file, key_file, ca_cert_file)
}
