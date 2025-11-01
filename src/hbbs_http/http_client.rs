use hbb_common::config::Config;
use hbb_common::log::info;
use hbb_common::proxy::{Proxy, ProxyScheme};
use reqwest::blocking::Client as SyncClient;
use reqwest::Client as AsyncClient;

macro_rules! configure_http_client {
    ($builder:expr, $Client: ty) => {{
        // https://github.com/rustdesk/rustdesk/issues/11569
        // https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html#method.no_proxy
        let mut builder = $builder.no_proxy();
        #[cfg(any(target_os = "android", target_os = "ios"))]
        match hbb_common::verifier::client_config() {
            Ok(client_config) => {
                builder = builder.use_preconfigured_tls(client_config);
            }
            Err(e) => {
                hbb_common::log::error!("Failed to get client config: {}", e);
            }
        }
        let client = if let Some(conf) = Config::get_socks() {
            let proxy_result = Proxy::from_conf(&conf, None);

            match proxy_result {
                Ok(proxy) => {
                    let proxy_setup = match &proxy.intercept {
                        ProxyScheme::Http { host, .. } => {
                            reqwest::Proxy::all(format!("http://{}", host))
                        }
                        ProxyScheme::Https { host, .. } => {
                            reqwest::Proxy::all(format!("https://{}", host))
                        }
                        ProxyScheme::Socks5 { addr, .. } => {
                            reqwest::Proxy::all(&format!("socks5://{}", addr))
                        }
                    };

                    match proxy_setup {
                        Ok(mut p) => {
                            if let Some(auth) = proxy.intercept.maybe_auth() {
                                if !auth.username().is_empty() && !auth.password().is_empty() {
                                    p = p.basic_auth(auth.username(), auth.password());
                                }
                            }
                            builder = builder.proxy(p);
                            builder.build().unwrap_or_else(|e| {
                                info!("Failed to create a proxied client: {}", e);
                                <$Client>::new()
                            })
                        }
                        Err(e) => {
                            info!("Failed to set up proxy: {}", e);
                            <$Client>::new()
                        }
                    }
                }
                Err(e) => {
                    info!("Failed to configure proxy: {}", e);
                    <$Client>::new()
                }
            }
        } else {
            builder.build().unwrap_or_else(|e| {
                info!("Failed to create a client: {}", e);
                <$Client>::new()
            })
        };

        client
    }};
}

pub fn create_http_client() -> SyncClient {
    let builder = SyncClient::builder();
    configure_http_client!(builder, SyncClient)
}

pub fn create_http_client_async() -> AsyncClient {
    let builder = AsyncClient::builder();
    configure_http_client!(builder, AsyncClient)
}
