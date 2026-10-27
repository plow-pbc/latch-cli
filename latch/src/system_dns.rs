//! The DNS every latch endpoint uses: the system's resolver
//! (`getaddrinfo`), the one every other program here asks. iroh's own
//! resolver queries the system's primary nameservers itself, over UDP; on a
//! Mac whose VPN carries DNS (a Tailscale exit node, say) that UDP can go
//! unanswered while the system resolves at once. iroh then waits on each
//! lookup before trying TCP, and its relay probes, which have three seconds
//! in all, end before they have measured a relay: the endpoint never
//! comes online. latch looks up only relay names, so no TXT records.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;

use iroh::dns::{BoxIter, DnsError, DnsResolver, Resolver, TxtRecordData};
use n0_error::AnyError;

/// What iroh's `Resolver` returns.
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Debug, Clone, Copy)]
struct System;

/// A resolver that asks the system's.
pub fn resolver() -> DnsResolver {
    DnsResolver::custom(System)
}

/// `host`'s addresses, as the system resolves them.
async fn lookup(host: String) -> Result<Vec<IpAddr>, DnsError> {
    let found = tokio::net::lookup_host((host.as_str(), 0))
        .await
        .map_err(|err| DnsError::from(AnyError::from_display(format!("{host}: {err}"))))?;
    Ok(found.map(|addr| addr.ip()).collect())
}

impl Resolver for System {
    fn lookup_ipv4(&self, host: String) -> BoxFuture<Result<BoxIter<Ipv4Addr>, DnsError>> {
        Box::pin(async move {
            let found = lookup(host).await?;
            let v4: Vec<Ipv4Addr> = found
                .into_iter()
                .filter_map(|ip| match ip {
                    IpAddr::V4(ip) => Some(ip),
                    IpAddr::V6(_) => None,
                })
                .collect();
            Ok(Box::new(v4.into_iter()) as BoxIter<Ipv4Addr>)
        })
    }

    fn lookup_ipv6(&self, host: String) -> BoxFuture<Result<BoxIter<Ipv6Addr>, DnsError>> {
        Box::pin(async move {
            let found = lookup(host).await?;
            let v6: Vec<Ipv6Addr> = found
                .into_iter()
                .filter_map(|ip| match ip {
                    IpAddr::V6(ip) => Some(ip),
                    IpAddr::V4(_) => None,
                })
                .collect();
            Ok(Box::new(v6.into_iter()) as BoxIter<Ipv6Addr>)
        })
    }

    fn lookup_txt(&self, host: String) -> BoxFuture<Result<BoxIter<TxtRecordData>, DnsError>> {
        Box::pin(async move {
            let why = format!("{host}: this host looks up no TXT records");
            Err(DnsError::from(AnyError::from_display(why)))
        })
    }

    fn clear_cache(&self) {}

    fn reset(&self) -> Box<dyn Resolver> {
        Box::new(System)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use iroh::RelayMode;

    use super::*;

    const WITHIN: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn names_are_looked_up_as_the_system_does_and_txt_records_never() {
        let dns = resolver();
        let found: Vec<IpAddr> = dns
            .lookup_ipv4("localhost", WITHIN)
            .await
            .unwrap()
            .collect();
        assert_eq!(found, [IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        assert!(
            dns.lookup_ipv4("no-such-host.invalid", WITHIN)
                .await
                .is_err()
        );
        assert!(dns.lookup_txt("localhost", WITHIN).await.is_err());
    }

    /// Every endpoint latch makes looks relay names up with the system's
    /// resolver, and reaches its relay by name.
    #[tokio::test]
    async fn an_endpoint_reaches_its_relay_by_the_systems_dns() {
        use iroh_relay::server::{RelayConfig, Server, ServerConfig};
        let mut config = ServerConfig::default();
        config.relay = Some(RelayConfig::new(([127, 0, 0, 1], 0)));
        let server = Server::spawn(config).await.unwrap();
        let port = server.http_addr().unwrap().port();
        let relay: iroh::RelayUrl = format!("http://localhost:{port}").parse().unwrap();

        let endpoint = crate::dial::endpoint()
            .relay_mode(RelayMode::custom([relay]))
            .bind()
            .await
            .unwrap();
        let txt = endpoint
            .dns_resolver()
            .unwrap()
            .lookup_txt("localhost", WITHIN)
            .await;
        let refused = txt.err().map(|err| format!("{err:#}")).unwrap_or_default();
        assert!(refused.contains("looks up no TXT records"), "{refused}");
        let online = tokio::time::timeout(Duration::from_secs(15), endpoint.online()).await;
        endpoint.close().await;
        assert!(online.is_ok(), "it never reached its relay");
    }
}
