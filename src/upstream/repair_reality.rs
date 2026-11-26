use super::{line::normalized_inbound, share_link, PanelApi};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};

// 3x-ui updates the entire inbound, including its embedded client record.
// The normalized client list can be newer if the panel was edited during our read.
fn client_settings_agree(embedded: &Value, client: &Value) -> bool {
    ["comment", "subId"].iter().all(|field| {
        embedded[field].as_str().unwrap_or_default() == client[field].as_str().unwrap_or_default()
    }) && ["expiryTime", "totalGB", "limitIp", "reset"]
        .iter()
        .all(|field| {
            embedded[field].as_i64().unwrap_or_default()
                == client[field].as_i64().unwrap_or_default()
        })
        && embedded["enable"].as_bool() == client["enable"].as_bool()
}

impl PanelApi {
    pub async fn repair_line_reality(
        &self,
        id: u64,
        name: &str,
        uuid: &str,
        host: &str,
        target: &str,
    ) -> Result<(String, usize)> {
        // Probe first. Read editable state after the probe so a slow TLS
        // handshake doesn't leave a stale inbound waiting to be written.
        let names = self.reality_names(target, None).await?;
        let preferred = names.first().context("面板未返回可用 SNI")?;
        let (raw, clients) = tokio::try_join!(self.inbound(id), self.clients())?;
        let mut inbound = normalized_inbound(&raw)?;
        let stream = &inbound["streamSettings"];
        let bound: Vec<_> = clients
            .iter()
            .filter(|c| {
                c["inboundIds"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&json!(id)))
            })
            .collect();
        ensure!(
            inbound["remark"] == name
                && inbound["tag"] == name
                && inbound["protocol"] == "vless"
                && stream["security"] == "reality"
                && matches!(stream["network"].as_str(), Some("tcp" | "raw"))
                && inbound["settings"]["clients"]
                    .as_array()
                    .is_some_and(|rows| rows.len() == 1
                        && rows[0]["id"] == uuid
                        && rows[0]["email"] == name)
                && bound.len() == 1
                && bound[0]["email"] == name
                && bound[0]["uuid"] == uuid,
            "此操作仅适用于单个同名客户端独占的 VLESS TCP Reality 入站，请同步并核对线路"
        );
        let embedded = &inbound["settings"]["clients"][0];
        ensure!(
            client_settings_agree(embedded, bound[0]),
            "入站内的客户端配置与面板记录不一致，请同步并核对客户端后重试"
        );
        let flow = embedded["flow"].as_str().unwrap_or_default().to_owned();
        let reality = &mut inbound["streamSettings"]["realitySettings"];
        let old = reality.clone();
        let settings = reality.as_object_mut().context("入站缺少 Reality 配置")?;
        settings.remove("dest");
        reality["target"] = json!(target);
        reality["serverNames"] = json!(names);
        reality["settings"]["serverName"] = json!(preferred);
        let changed = *reality != old;
        let link = share_link(&inbound, name, uuid, host, &flow)?;
        if changed {
            self.save_inbound(id, inbound).await?;
        }
        Ok((link, names.len()))
    }
}
