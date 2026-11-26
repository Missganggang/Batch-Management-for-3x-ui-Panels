use super::PanelApi;
use anyhow::{anyhow, ensure, Context, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeMode {
    Tcp,
    Http,
}

impl ProbeMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Http => "http",
        }
    }
}

type Measurement = Result<i64, &'static str>;

// An upstream error can contain the test URL, proxy credentials or Xray logs.
fn probe_error(reason: &str, mode: ProbeMode) -> &'static str {
    let text = reason.to_ascii_lowercase();
    if text.contains("another outbound test") {
        return "该面板正在进行其他出站测试，请稍后重试";
    }
    if text.contains("timeout") || text.contains("timed out") || text.contains("deadline exceeded")
    {
        return "测试连接超时";
    }
    if text.contains("connection refused") || text.contains("actively refused") {
        return "目标端口拒绝连接";
    }
    if text.contains("no such host") {
        return "无法解析测试目标的域名";
    }
    if text.contains("no testable endpoint") {
        return "出站缺少可测试的服务器地址和端口";
    }
    match mode {
        ProbeMode::Tcp => "TCP 连接失败，请核对 SOCKS5 出口地址和端口",
        ProbeMode::Http => "HTTP 测试失败，请核对 SOCKS5 出口、认证信息及面板的测试网址",
    }
}

fn measurement(reply: &Value, expected_tag: &str, mode: ProbeMode) -> Measurement {
    if reply["tag"] != expected_tag || !reply["success"].is_boolean() {
        return Err("面板返回的测试结果与请求不符");
    }
    if reply["success"] != true {
        return Err(probe_error(
            reply["error"].as_str().unwrap_or_default(),
            mode,
        ));
    }
    reply["delay"]
        .as_i64()
        .filter(|ms| *ms >= 0)
        .ok_or("面板未返回有效的出口延迟")
}

impl PanelApi {
    pub async fn test_socks_outbound(&self, tag: &str, mode: ProbeMode) -> Result<i64> {
        self.test_socks_batch(&[tag], mode)
            .await?
            .pop()
            .context("面板未返回出站测试结果")?
            .map_err(|reason| anyhow!(reason))
    }

    pub async fn test_socks_batch(
        &self,
        tags: &[&str],
        mode: ProbeMode,
    ) -> Result<Vec<Measurement>> {
        let config = self
            .config()
            .await
            .map_err(|_| anyhow!("无法读取面板出站配置，请检查 API 连接与令牌"))?;
        let outbounds = config["outbounds"].as_array().context("面板缺少出站列表")?;
        let mut measurements = vec![Err("面板找不到该出站，请刷新列表"); tags.len()];
        let mut selected = Vec::new();
        for (i, tag) in tags.iter().enumerate() {
            let mut matches = outbounds.iter().filter(|o| o["tag"].as_str() == Some(*tag));
            let Some(outbound) = matches.next() else {
                continue;
            };
            if matches.next().is_some() {
                measurements[i] = Err("面板包含重复的出站标签");
            } else if outbound["protocol"] != "socks" {
                measurements[i] = Err("此出站未使用 SOCKS5 协议");
            } else {
                selected.push((i, outbound));
            }
        }
        if selected.is_empty() {
            return Ok(measurements);
        }
        let batch = tags.len() > 1;
        let (endpoint, field, payload) = if batch {
            let rows: Vec<_> = selected.iter().map(|(_, outbound)| outbound).collect();
            (
                "xray/testOutbounds",
                "outbounds",
                serde_json::to_string(&rows)?,
            )
        } else {
            ("xray/testOutbound", "outbound", selected[0].1.to_string())
        };
        let request = self
            .request(Method::POST, endpoint)
            .timeout(Duration::from_secs(90))
            .form(&[
                (field, payload),
                ("allOutbounds", config["outbounds"].to_string()),
                ("mode", mode.as_str().to_owned()),
            ]);
        let envelope: Value = self
            .send(request)
            .await?
            .json()
            .await
            .map_err(|_| anyhow!("面板未返回有效的出站测试响应"))?;
        ensure!(
            envelope["success"] == true,
            "面板无法执行出站测试，请核对出站测试网址和面板状态"
        );
        let replies = if batch {
            envelope["obj"]
                .as_array()
                .context("面板未返回批量测试列表")?
                .as_slice()
        } else {
            std::slice::from_ref(&envelope["obj"])
        };
        ensure!(
            replies.len() == selected.len(),
            "面板返回的出站测试结果数量不符"
        );
        for ((index, _), reply) in selected.into_iter().zip(replies) {
            measurements[index] = measurement(reply, tags[index], mode);
        }
        Ok(measurements)
    }
}
