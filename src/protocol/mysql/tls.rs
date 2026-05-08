//! TLS helpers for the MySQL protocol layer.
//!
//! Provides two public entry points:
//! - `build_backend_connector` — build a `TlsConnector` for encrypted backend connections
//! - `build_frontend_acceptor` — build a `TlsAcceptor` for accepting TLS clients

use std::fs;
use std::io::BufReader;
use std::sync::Arc;

use tokio_rustls::rustls::{self, ClientConfig, RootCertStore, ServerConfig};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::config::{FrontendTlsConfig, TlsMode};

// ─── NoVerify ────────────────────────────────────────────────────────────────

/// A certificate verifier that accepts any server certificate.
/// Used for `TlsMode::Required` (encrypt without cert verification).
#[derive(Debug)]
struct NoVerify;

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA1,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ED448,
        ]
    }
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Build a `TlsConnector` for backend (outgoing) connections.
///
/// | `tls_mode`       | Behaviour                                              |
/// |------------------|--------------------------------------------------------|
/// | `required`       | Encrypt; no cert verification (≈ `VERIFY_NOTHING`)    |
/// | `verify-ca`      | Verify against `tls_ca` or Mozilla root store          |
/// | `verify-identity`| Verify cert **and** hostname                           |
///
/// Panics if called with `TlsMode::Off`.
pub fn build_backend_connector(
    tls_mode: &TlsMode,
    tls_ca: Option<&str>,
) -> anyhow::Result<TlsConnector> {
    let client_config: ClientConfig = match tls_mode {
        TlsMode::Off => unreachable!("build_backend_connector called with TlsMode::Off"),

        TlsMode::Required => {
            // Encrypt the connection but skip certificate verification.
            // Equivalent to MySQL ssl-mode=REQUIRED without cert check.
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify))
                .with_no_client_auth()
        }

        TlsMode::VerifyCa | TlsMode::VerifyIdentity => {
            let mut root_store = RootCertStore::empty();

            if let Some(ca_path) = tls_ca {
                // Load custom CA bundle.
                let ca_bytes = fs::read(ca_path)
                    .map_err(|e| anyhow::anyhow!("TLS CA '{}': {}", ca_path, e))?;
                let mut reader = BufReader::new(ca_bytes.as_slice());
                for cert in rustls_pemfile::certs(&mut reader) {
                    let cert = cert
                        .map_err(|e| anyhow::anyhow!("Invalid cert in '{}': {}", ca_path, e))?;
                    root_store
                        .add(cert)
                        .map_err(|e| anyhow::anyhow!("Malformed CA cert: {}", e))?;
                }
            } else {
                // Fall back to the Mozilla/webpki root store.
                root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            }

            ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth()
        }
    };

    Ok(TlsConnector::from(Arc::new(client_config)))
}

/// Build a `TlsAcceptor` for frontend (incoming) TLS connections.
///
/// `cert` must be a path to a PEM certificate chain; `key` a PEM private key.
pub fn build_frontend_acceptor(config: &FrontendTlsConfig) -> anyhow::Result<TlsAcceptor> {
    if config.cert.is_empty() || config.key.is_empty() {
        anyhow::bail!("frontend_tls.cert and frontend_tls.key must both be set when frontend_tls.enabled = true");
    }

    let cert_bytes = fs::read(&config.cert)
        .map_err(|e| anyhow::anyhow!("TLS cert '{}': {}", config.cert, e))?;
    let key_bytes = fs::read(&config.key)
        .map_err(|e| anyhow::anyhow!("TLS key '{}': {}", config.key, e))?;

    let certs: Vec<CertificateDer<'static>> = {
        let mut r = BufReader::new(cert_bytes.as_slice());
        rustls_pemfile::certs(&mut r).collect::<Result<_, _>>()?
    };

    let private_key: PrivateKeyDer<'static> = {
        let mut r = BufReader::new(key_bytes.as_slice());
        rustls_pemfile::private_key(&mut r)?
            .ok_or_else(|| anyhow::anyhow!("No private key found in '{}'", config.key))?
    };

    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, private_key)
        .map_err(|e| anyhow::anyhow!("Invalid TLS cert/key: {}", e))?;

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}
