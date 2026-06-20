//! TLS acceptor setup. The listener integration lives in `mod.rs`.

use std::fs;

use native_tls::{Identity, TlsAcceptor};

use super::{TlsIdentity, TlsSettings};

pub fn build_acceptor(settings: &TlsSettings) -> Option<TlsAcceptor> {
    let identity = match &settings.identity {
        TlsIdentity::Pkcs12 { path, password } => {
            let bytes = match fs::read(path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    eprintln!(
                        "bolide web TLS: failed to read PKCS#12 identity '{}': {}",
                        path, err
                    );
                    return None;
                }
            };
            match Identity::from_pkcs12(&bytes, password) {
                Ok(identity) => identity,
                Err(err) => {
                    eprintln!(
                        "bolide web TLS: failed to load PKCS#12 identity '{}': {}",
                        path, err
                    );
                    return None;
                }
            }
        }
        TlsIdentity::Pkcs8 {
            cert_path,
            key_path,
        } => {
            let cert = match fs::read(cert_path) {
                Ok(cert) => cert,
                Err(err) => {
                    eprintln!(
                        "bolide web TLS: failed to read PEM certificate '{}': {}",
                        cert_path, err
                    );
                    return None;
                }
            };
            let key = match fs::read(key_path) {
                Ok(key) => key,
                Err(err) => {
                    eprintln!(
                        "bolide web TLS: failed to read PKCS#8 private key '{}': {}",
                        key_path, err
                    );
                    return None;
                }
            };
            match Identity::from_pkcs8(&cert, &key) {
                Ok(identity) => identity,
                Err(err) => {
                    eprintln!(
                        "bolide web TLS: failed to load PKCS#8 identity '{}', '{}': {}",
                        cert_path, key_path, err
                    );
                    return None;
                }
            }
        }
    };
    match TlsAcceptor::new(identity) {
        Ok(acceptor) => Some(acceptor),
        Err(err) => {
            eprintln!("bolide web TLS: failed to create TLS acceptor: {}", err);
            None
        }
    }
}
