use super::PanelApi;
use anyhow::{ensure, Context, Result};
use reqwest::Method;
use serde_json::Value;
use std::{collections::HashSet, net::IpAddr};

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.parse::<IpAddr>().is_err()
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn host_port(target: &str) -> Option<(&str, u16)> {
    let (host, port) = target.rsplit_once(':')?;
    let port = port.parse::<u16>().ok().filter(|p| *p > 0)?;
    if let Some(ip) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        ip.parse::<std::net::Ipv6Addr>().ok()?;
        Some((ip, port))
    } else if valid_name(host) || host.parse::<std::net::Ipv4Addr>().is_ok() {
        Some((host, port))
    } else {
        None
    }
}

impl PanelApi {
    pub(super) async fn reality_names(
        &self,
        target: &str,
        preferred: Option<&str>,
    ) -> Result<Vec<String>> {
        let (host, port) =
            host_port(target).context("Reality 目标须为单个主机和端口，例如 www.amd.com:443")?;
        if let Some(name) = preferred {
            ensure!(valid_name(name), "指定的 Reality SNI 域名无效");
        }
        let result = self
            .obj(
                self.request(Method::POST, "server/scanRealityTarget")
                    .form(&[("target", target)]),
            )
            .await
            .with_context(|| format!("面板检测 Reality 目标 {target} 失败"))?;
        ensure!(
            result["feasible"] == true,
            "面板检测 Reality 目标 {target} 未通过：{}",
            result["reason"]
                .as_str()
                .filter(|r| !r.is_empty())
                .unwrap_or("目标未满足 TLS 1.3、HTTP/2、X25519 或证书要求")
        );
        ensure!(
            result["certValid"] == true
                && result["host"]
                    .as_str()
                    .is_some_and(|h| h.eq_ignore_ascii_case(host))
                && result["port"].as_u64() == Some(port.into()),
            "面板未确认指定 Reality 目标的证书"
        );
        let mut names = Vec::new();
        let mut seen = HashSet::new();
        // certValid verifies the dialled hostname, including wildcard coverage. 3x-ui
        // filters wildcard SANs out of serverNames, so keep that verified concrete host.
        for name in std::iter::once(host).chain(
            result["serverNames"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        ) {
            let name = name.trim().to_ascii_lowercase();
            if valid_name(&name) && seen.insert(name.clone()) {
                names.push(name);
            }
        }
        ensure!(!names.is_empty(), "Reality 目标的证书缺少可用 SNI");
        if let Some(name) = preferred {
            let index = names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(name))
                .context("指定的 SNI 未通过目标主机名验证，也未出现在面板返回的证书域名中")?;
            names[..=index].rotate_right(1);
        }
        Ok(names)
    }
}

pub(super) fn share_sni(reality: &Value) -> Result<&str> {
    let names: Vec<&str> = reality["serverNames"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|s| valid_name(s))
        .collect();
    let preferred = reality["settings"]["serverName"].as_str();
    let target = ["target", "dest"]
        .into_iter()
        .find_map(|key| host_port(reality[key].as_str()?).map(|(host, _)| host));
    [preferred, target]
        .into_iter()
        .flatten()
        .find_map(|candidate| {
            names
                .iter()
                .copied()
                .find(|s| s.eq_ignore_ascii_case(candidate))
        })
        .or_else(|| names.first().copied())
        .context("入站缺少可用 SNI，请检查 Reality serverNames")
}
