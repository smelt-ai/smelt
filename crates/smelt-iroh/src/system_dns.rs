//! 让 iroh 走操作系统的 DNS 解析器（getaddrinfo）。
//!
//! iroh 自带的解析器自己读系统 DNS 配置、自己发 UDP 查询。在 Apple 平台上它读
//! SystemConfiguration 的 `State:/Network/Global/DNS`，而这组 API 在 iOS 上是
//! `API_UNAVAILABLE(ios)`——读取必然失败，于是悄悄退回写死的 Cloudflare / Google /
//! Quad9，直接往 1.1.1.1:53 之类发包。这就绕开了系统的 VPN / 代理 / 分域 DNS：
//! 手机开着规则代理（Stash 等）时，这些查询被当成普通 IP 流量按规则分流，relay 域名
//! 解析不出来，连接一直超时，而同一台手机上 Safari 打开 relay 却完全正常。
//!
//! 交给系统解析器后，iroh 与普通 App 看到同一套 DNS，代理、VPN、分域解析、hosts
//! 都自然生效；桌面端也因此能吃到 macOS 的分域解析器（iroh 自带的那套不读）。
//!
//! 只实现 A/AAAA。`Minimal` 预设不装 DNS 地址发现，iroh 不会发 TXT 查询。

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;

use iroh::dns::{BoxIter, DnsError, Resolver, TxtRecordData};
use n0_error::AnyError;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// 委托给操作系统解析器（tokio 的 `lookup_host`，底层是 getaddrinfo）。
///
/// 不自带缓存：系统解析器自己会缓存，`clear_cache` / `reset` 因此都无事可做。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SystemResolver;

impl SystemResolver {
    async fn lookup(host: String) -> Result<Vec<IpAddr>, DnsError> {
        let addrs = tokio::net::lookup_host((host.as_str(), 0))
            .await
            .map_err(|err| DnsError::from(AnyError::from(err)))?;
        Ok(addrs.map(|addr| addr.ip()).collect())
    }
}

impl Resolver for SystemResolver {
    fn lookup_ipv4(&self, host: String) -> BoxFuture<Result<BoxIter<Ipv4Addr>, DnsError>> {
        Box::pin(async move {
            let v4: Vec<Ipv4Addr> = Self::lookup(host)
                .await?
                .into_iter()
                .filter_map(|ip| match ip {
                    IpAddr::V4(v4) => Some(v4),
                    IpAddr::V6(_) => None,
                })
                .collect();
            Ok(Box::new(v4.into_iter()) as BoxIter<Ipv4Addr>)
        })
    }

    fn lookup_ipv6(&self, host: String) -> BoxFuture<Result<BoxIter<Ipv6Addr>, DnsError>> {
        Box::pin(async move {
            let v6: Vec<Ipv6Addr> = Self::lookup(host)
                .await?
                .into_iter()
                .filter_map(|ip| match ip {
                    IpAddr::V6(v6) => Some(v6),
                    IpAddr::V4(_) => None,
                })
                .collect();
            Ok(Box::new(v6.into_iter()) as BoxIter<Ipv6Addr>)
        })
    }

    fn lookup_txt(&self, host: String) -> BoxFuture<Result<BoxIter<TxtRecordData>, DnsError>> {
        Box::pin(async move {
            Err(DnsError::from(AnyError::from(format!(
                "系统解析器不支持 TXT 查询（{host}）"
            ))))
        })
    }

    fn clear_cache(&self) {}

    fn reset(&self) -> Box<dyn Resolver> {
        Box::new(*self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_ip_literals_by_family() {
        let r = SystemResolver;
        let v4: Vec<_> = r.lookup_ipv4("127.0.0.1".into()).await.unwrap().collect();
        assert_eq!(v4, [Ipv4Addr::LOCALHOST]);
        let v6: Vec<_> = r.lookup_ipv6("::1".into()).await.unwrap().collect();
        assert_eq!(v6, [Ipv6Addr::LOCALHOST]);
        // 族不匹配时是空结果，不是错误：iroh 会把 A/AAAA 两路结果合并。
        let none: Vec<_> = r.lookup_ipv6("127.0.0.1".into()).await.unwrap().collect();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn resolves_localhost_through_the_os() {
        let v4: Vec<_> = SystemResolver
            .lookup_ipv4("localhost".into())
            .await
            .unwrap()
            .collect();
        assert!(v4.contains(&Ipv4Addr::LOCALHOST), "{v4:?}");
    }

    #[tokio::test]
    async fn txt_lookup_is_an_error() {
        assert!(
            SystemResolver
                .lookup_txt("example.test".into())
                .await
                .is_err()
        );
    }
}
