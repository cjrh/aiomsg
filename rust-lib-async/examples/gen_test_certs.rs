//! Generate the shared self-signed certificate used by the cross-language TLS
//! conformance scenarios. The certificate is its own trust anchor (clients
//! trust `cert.pem` directly) and carries both `DNS:localhost` and
//! `IP:127.0.0.1` SANs so every language's verifier accepts it when connecting
//! to the loopback address.
//!
//! Run from the repo root to (re)generate the committed fixtures:
//!
//! ```text
//! cargo run --manifest-path rust-lib-async/Cargo.toml \
//!     --example gen_test_certs -- conformance/certs
//! ```

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use rcgen::{CertificateParams, DnType, KeyPair, SanType};

fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: gen_test_certs <out_dir>"),
    );

    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params.subject_alt_names = vec![
        SanType::DnsName("localhost".try_into().unwrap()),
        SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
    ];
    params
        .distinguished_name
        .push(DnType::CommonName, "aiomsg conformance test");
    // Far-future expiry so the committed fixture does not rot.
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(4096, 1, 1);

    let key = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();

    std::fs::write(out.join("cert.pem"), cert.pem()).unwrap();
    std::fs::write(out.join("key.pem"), key.serialize_pem()).unwrap();
    println!("wrote cert.pem and key.pem to {}", out.display());
}
