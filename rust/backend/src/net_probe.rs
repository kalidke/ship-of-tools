//! `sotd net-probe <https-url> <wss-url>`: the web stack the project chat will use, exercised once, on a measuring
//! branch only (it never lands). One rustls client configuration (the ring provider, Mozilla's bundled roots, no
//! system OpenSSL) serves an HTTPS GET through reqwest, with redirects and ambient proxies off, and a websocket
//! handshake through tokio-tungstenite. It holds no chat code; it keeps the stack linked so the daemon's growth can be
//! measured and its TLS start proved on each system.

use std::sync::Arc;

pub(crate) const USAGE: &str = "usage: sotd net-probe <https-url> <wss-url>";

/// The one client configuration both transports share.
fn client_config() -> anyhow::Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

pub(crate) async fn run(args: &[String]) -> i32 {
    let [https, wss] = args else {
        eprintln!("{USAGE}");
        return 2;
    };
    match probe(https, wss).await {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(e) => {
            eprintln!("net-probe: {e:#}");
            1
        }
    }
}

async fn probe(https: &str, wss: &str) -> anyhow::Result<String> {
    let config = client_config()?;
    let client = reqwest::Client::builder()
        .tls_backend_preconfigured(config.clone())
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()?;
    let status = client.get(https).send().await?.status();
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(config));
    let (socket, response) = tokio_tungstenite::connect_async_tls_with_config(wss, None, false, Some(connector)).await?;
    drop(socket);
    Ok(format!("https {status}; websocket {}", response.status()))
}
