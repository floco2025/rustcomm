use crate::error::Error;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use rustls_pemfile::{certs, private_key};
use std::fs::File;
use std::io::BufReader;

pub(crate) fn load_tls_server_config(
    cert_path: &str,
    key_path: &str,
) -> Result<ServerConfig, Error> {
    let cert_file = File::open(cert_path).map_err(|e| Error::TlsCertificateLoad {
        path: cert_path.to_string(),
        source: e,
    })?;
    let cert_chain: Vec<CertificateDer> = certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| Error::TlsInvalidCertificate(format!("Failed to parse certificates: {e}")))?;

    if cert_chain.is_empty() {
        return Err(Error::TlsInvalidCertificate(
            "No certificates found in file".to_string(),
        ));
    }

    let key_file = File::open(key_path).map_err(|e| Error::TlsKeyLoad {
        path: key_path.to_string(),
        source: e,
    })?;
    let key = private_key(&mut BufReader::new(key_file))
        .map_err(|e| Error::TlsInvalidKey(format!("Failed to parse private key: {e}")))?
        .ok_or_else(|| Error::TlsInvalidKey("No private key found in file".to_string()))?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| Error::TlsServerConfigBuild(e.to_string()))?;

    Ok(config)
}

pub(crate) fn load_tls_client_config(ca_cert_path: &str) -> Result<ClientConfig, Error> {
    let ca_cert_file = File::open(ca_cert_path).map_err(|e| Error::TlsCertificateLoad {
        path: ca_cert_path.to_string(),
        source: e,
    })?;
    let ca_certs: Vec<CertificateDer> = certs(&mut BufReader::new(ca_cert_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            Error::TlsInvalidCertificate(format!("Failed to parse CA certificates: {e}"))
        })?;

    if ca_certs.is_empty() {
        return Err(Error::TlsInvalidCertificate(
            "No CA certificates found in file".to_string(),
        ));
    }

    let mut root_cert_store = RootCertStore::empty();
    for cert in ca_certs {
        root_cert_store
            .add(cert)
            .map_err(|e| Error::TlsInvalidCertificate(e.to_string()))?;
    }

    let config = ClientConfig::builder()
        .with_root_certificates(root_cert_store)
        .with_no_client_auth();

    Ok(config)
}
