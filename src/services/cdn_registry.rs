use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const FAILURE_THRESHOLD: u32 = 2;
const BLOCK_TTL: Duration = Duration::from_secs(10 * 60);

/// 检测 URL 是否来自劣质 CDN 节点（MCDN/PCDN）。
/// B站的 MCDN 是 P2P 加速节点，常使用非标准端口（如 8082），
/// 其 SSL 证书链和连接稳定性普遍较差，容易触发 TLS 握手失败/拒连。
pub(crate) fn is_mcdn_url(url: &str) -> bool {
    const CDN_BLACKLIST: &[&str] = &["mcdn", "pcdn", "szbdyd.com", "mountaintoys.cn"];
    let url_lower = url.to_lowercase();
    CDN_BLACKLIST
        .iter()
        .any(|keyword| url_lower.contains(keyword))
}

#[derive(Clone, Debug)]
struct HostState {
    failures: u32,
    blocked_until: Option<Instant>,
}

#[derive(Default)]
pub struct BadCdnRegistry {
    hosts: RwLock<HashMap<String, HostState>>,
}

impl BadCdnRegistry {
    pub async fn record_failure(&self, host: &str) {
        let mut hosts = self.hosts.write().await;
        let state = hosts.entry(host.to_ascii_lowercase()).or_insert(HostState {
            failures: 0,
            blocked_until: None,
        });
        state.failures = state.failures.saturating_add(1);
        if state.failures >= FAILURE_THRESHOLD {
            state.blocked_until = Some(Instant::now() + BLOCK_TTL);
        }
    }

    pub async fn record_success(&self, host: &str) {
        self.hosts.write().await.remove(&host.to_ascii_lowercase());
    }

    pub async fn is_blocked(&self, host: &str) -> bool {
        let key = host.to_ascii_lowercase();
        let now = Instant::now();
        {
            let hosts = self.hosts.read().await;
            if let Some(state) = hosts.get(&key) {
                if state.blocked_until.is_some_and(|until| until > now) {
                    return true;
                }
            }
        }
        let mut hosts = self.hosts.write().await;
        if hosts
            .get(&key)
            .and_then(|state| state.blocked_until)
            .is_some_and(|until| until <= now)
        {
            hosts.remove(&key);
        }
        false
    }

    /// 从候选 URL 中选择下载地址：
    /// 1. 优先未熔断且非 MCDN/PCDN 的节点（劣质节点常见拒连/TLS 失败）；
    /// 2. 其次未熔断的 MCDN 节点；
    /// 3. 兜底返回首个候选。
    pub async fn choose_url<'a>(&self, urls: &'a [String]) -> Option<&'a str> {
        let mut mcdn_fallback: Option<&'a str> = None;
        for url in urls {
            let host = url::Url::parse(url)
                .ok()
                .and_then(|parsed| parsed.host_str().map(str::to_owned));
            if let Some(host) = host {
                if !self.is_blocked(&host).await {
                    if !is_mcdn_url(url) {
                        return Some(url);
                    }
                    mcdn_fallback.get_or_insert(url);
                }
            }
        }
        mcdn_fallback.or(urls.first().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blocks_after_two_failures_and_success_clears() {
        let registry = BadCdnRegistry::default();
        registry.record_failure("cdn.example").await;
        assert!(!registry.is_blocked("cdn.example").await);
        registry.record_failure("cdn.example").await;
        assert!(registry.is_blocked("cdn.example").await);
        registry.record_success("cdn.example").await;
        assert!(!registry.is_blocked("cdn.example").await);
    }

    #[tokio::test]
    async fn choose_url_prefers_normal_cdn_over_mcdn() {
        let registry = BadCdnRegistry::default();
        let urls = vec![
            "https://xy1x2xy.mcdn.bilivideo.cn:8082/v1/a.m4s".to_string(),
            "https://upos-sz-mirror.bilivideo.com/a.m4s".to_string(),
        ];
        assert_eq!(
            registry.choose_url(&urls).await,
            Some("https://upos-sz-mirror.bilivideo.com/a.m4s")
        );

        // 正常节点全部熔断后，回退到未熔断的 MCDN 节点
        registry
            .record_failure("upos-sz-mirror.bilivideo.com")
            .await;
        registry
            .record_failure("upos-sz-mirror.bilivideo.com")
            .await;
        assert_eq!(
            registry.choose_url(&urls).await,
            Some("https://xy1x2xy.mcdn.bilivideo.cn:8082/v1/a.m4s")
        );
    }
}
