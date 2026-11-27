use crate::{storage, ApiError, ApiResult, Shared};
use anyhow::{Context, Result};
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    Json,
};
use chrono::{DateTime, Months, Utc};
use futures_util::{stream, StreamExt};
use qrcode::{render::svg, QrCode};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

fn expiry(base: i64, months: u32) -> ApiResult<i64> {
    if !matches!(months, 1 | 2 | 3 | 6 | 12) {
        return Err(ApiError::bad("请选择 1、2、3、6 个月或 1 年"));
    }
    DateTime::<Utc>::from_timestamp_millis(base)
        .and_then(|d| d.checked_add_months(Months::new(months)))
        .map(|d| d.timestamp_millis())
        .ok_or_else(|| ApiError::bad("到期时间超出范围"))
}

pub(crate) fn public_host(value: &str) -> ApiResult<String> {
    let host = value.trim();
    if host.is_empty() {
        return Ok(String::new());
    }
    let invalid = || ApiError::bad("连接地址只需填写域名或 IP，端口由系统分配");
    if host.len() > 253
        || host
            .chars()
            .any(|c| c.is_whitespace() || "/@?#%\\".contains(c))
    {
        return Err(invalid());
    }
    let mut url = Url::parse("http://localhost").expect("static URL");
    let plain = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    let input = if let Ok(ip) = plain.parse::<std::net::Ipv6Addr>() {
        format!("[{ip}]")
    } else {
        if host.contains([':', '[', ']']) {
            return Err(invalid());
        }
        host.to_owned()
    };
    url.set_host(Some(&input)).map_err(|_| invalid())?;
    Ok(url.host_str().ok_or_else(invalid)?.to_string())
}

fn with_host(link: &str, host: &str) -> Result<String> {
    if host.is_empty() {
        return Ok(link.to_owned());
    }
    let mut url = Url::parse(link)?;
    let host = Url::parse(&format!("http://{host}"))?;
    // vless is a non-special URL scheme. Supply ASCII domains for client interoperability.
    url.set_host(host.host_str())?;
    Ok(url.into())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicHost {
    public_host: String,
}
pub async fn change_host(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(input): Json<PublicHost>,
) -> ApiResult<Json<Value>> {
    let host = public_host(&input.public_host)?;
    let _guard = app.lock(&id).await;
    let mut panel = app.panel(&id).await?;
    panel.public_host = host.clone();
    let mut tx = app.db.begin().await.context("无法更新连接地址")?;
    update_share_links(&mut tx, &panel).await?;
    sqlx::query("UPDATE panels SET revision=revision+(public_host<>?), public_host=? WHERE id=?")
        .bind(&host)
        .bind(&host)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .context("无法保存连接地址")?;
    tx.commit().await.context("无法保存连接地址")?;
    Ok(Json(json!({"ok":true,"publicHost":host})))
}

pub(crate) async fn update_share_links(
    tx: &mut sqlx::SqliteConnection,
    panel: &storage::Panel,
) -> Result<()> {
    let host = crate::sync::sharing_host(panel)?;
    let rows = sqlx::query("SELECT id,link FROM lines WHERE panel_id=? AND link IS NOT NULL")
        .bind(&panel.id)
        .fetch_all(&mut *tx)
        .await
        .context("无法读取线路")?;
    for row in rows {
        let link = with_host(row.get("link"), &host)?;
        sqlx::query("UPDATE lines SET link=? WHERE id=?")
            .bind(link)
            .bind(row.get::<String, _>("id"))
            .execute(&mut *tx)
            .await
            .context("无法更新分享链接")?;
    }
    Ok(())
}

const SELECT_LINES: &str =
    "SELECT l.*,p.name AS panel_name FROM lines l JOIN panels p ON p.id=l.panel_id";
fn line(row: &SqliteRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"name":row.get::<String,_>("name"),"panelId":row.get::<String,_>("panel_id"),
        "clientName":row.get::<String,_>("client_email"),"inboundName":row.get::<String,_>("inbound_name"),
        "inboundId":row.get::<Option<i64>,_>("inbound_id"),"sharedClient":row.get::<bool,_>("shared_client"),
        "enabled":row.get::<bool,_>("enabled"),"inboundExpiryTime":row.get::<i64,_>("inbound_expiry"),
        "panelName":row.get::<String,_>("panel_name"),"status":row.get::<String,_>("status"),"expiryTime":row.get::<i64,_>("expiry"),
        "quotaBytes":row.get::<Option<i64>,_>("quota_bytes"),
        "updatedAt":row.get::<i64,_>("updated"),"error":row.get::<Option<String>,_>("error"),"link":row.get::<Option<String>,_>("link"),
        "online":null,"up":null,"down":null})
}
async fn by_id(db: &SqlitePool, id: &str) -> ApiResult<SqliteRow> {
    sqlx::query(&format!("{SELECT_LINES} WHERE l.id=?"))
        .bind(id)
        .fetch_optional(db)
        .await
        .context("无法读取线路")?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "线路不存在".into()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateLine {
    panel_id: String,
    #[serde(flatten)]
    limits: CreationLimits,
    #[serde(flatten)]
    line: LineInput,
}

#[derive(Deserialize)]
struct LineInput {
    name: String,
    socks5: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBatch {
    panel_id: String,
    #[serde(flatten)]
    limits: CreationLimits,
    lines: Vec<LineInput>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct CreationLimits {
    months: u32,
    expiry_time: Option<i64>,
    quota_bytes: i64,
}

impl CreationLimits {
    fn resolve(&self) -> ApiResult<(i64, i64)> {
        if self.quota_bytes < 0 {
            return Err(ApiError::bad("流量上限须为非负整数字节，0 表示不限"));
        }
        let end = match (self.expiry_time, self.months) {
            (Some(end @ 0..=8_640_000_000_000_000), 0) => end,
            (Some(_), 0) => return Err(ApiError::bad("自定义到期时间超出范围")),
            (Some(_), _) => return Err(ApiError::bad("请选择快捷期限或自定义到期时间中的一项")),
            (None, 0) => 0,
            (None, months) => expiry(storage::now(), months)?,
        };
        Ok((end, self.quota_bytes))
    }
}

fn validate_line(input: &LineInput) -> ApiResult<()> {
    let name = input.name.trim();
    if name.is_empty()
        || name.chars().count() > 80
        || name
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '/' | '\\'))
    {
        return Err(ApiError::bad(
            "名称支持汉字、字母、数字和短横线，长度 1 至 80 个字符，请勿包含空格或斜杠",
        ));
    }
    if input.socks5.trim().is_empty() || input.socks5.len() > 4096 {
        return Err(ApiError::bad("请填写有效的 SOCKS5 地址"));
    }
    Ok(())
}

pub async fn create(
    State(app): State<Shared>,
    Json(input): Json<CreateLine>,
) -> ApiResult<Json<Value>> {
    validate_line(&input.line)?;
    let limits = input.limits.resolve()?;
    // Keep a network interruption in the browser from abandoning partially applied upstream work.
    let line = tokio::spawn(async move {
        let _guard = app.lock(&input.panel_id).await;
        let panel = app.panel(&input.panel_id).await?;
        let api = panel.api(&app.client)?;
        provision(&app, &panel, &api, &input.line, limits).await
    })
    .await
    .context("线路任务中断")??;
    Ok(Json(json!({"line":line})))
}

pub async fn create_batch(
    State(app): State<Shared>,
    Json(input): Json<CreateBatch>,
) -> ApiResult<Json<Value>> {
    if input.lines.is_empty() {
        return Err(ApiError::bad("请至少填写一条线路"));
    }
    let limits = input.limits.resolve()?;
    // A single panel's Xray template is replaced as a whole. Write its lines in
    // input order and isolate individual failures just as for single creation.
    tokio::spawn(async move {
        let _guard = app.lock(&input.panel_id).await;
        let panel = app.panel(&input.panel_id).await?;
        let api = panel.api(&app.client)?;
        let mut names = HashSet::new();
        let mut results = Vec::with_capacity(input.lines.len());
        for entry in &input.lines {
            let name = entry.name.trim();
            let result = match validate_line(entry) {
                Err(err) => Err(err),
                Ok(()) if !names.insert(name) => Err(ApiError::bad("该名称在本批线路中重复")),
                Ok(()) => provision(&app, &panel, &api, entry, limits).await,
            };
            results.push(match result {
                Ok(line) => json!({"name":name,"ok":true,"message":"已创建","line":line}),
                Err(err) => json!({"name":name,"ok":false,"message":err.1}),
            });
        }
        Ok(Json(json!({"results":results})))
    })
    .await
    .context("批量创建线路任务中断")?
}

async fn provision(
    app: &Shared,
    panel: &storage::Panel,
    api: &crate::PanelApi,
    input: &LineInput,
    (end, quota): (i64, i64),
) -> ApiResult<Value> {
    let name = input.name.trim();
    let id = Uuid::new_v4().to_string();
    let uid = Uuid::new_v4().to_string();
    let reserved:i64=sqlx::query_scalar("SELECT count(*) FROM lines WHERE panel_id=? AND status='ready' AND (client_email=? OR inbound_name=?)")
        .bind(&panel.id).bind(name).bind(name).fetch_one(&app.db).await.context("检查线路名称失败")?;
    if reserved > 0 {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "面板已存在同名入站或客户端".into(),
        ));
    }
    sqlx::query("INSERT INTO lines(id,panel_id,name,client_email,inbound_name,uuid,status,expiry,quota_bytes,updated) VALUES(?,?,?,?,?,?,'pending',?,?,?) ON CONFLICT DO NOTHING")
        .bind(&id).bind(&panel.id).bind(name).bind(name).bind(name).bind(&uid).bind(end).bind(quota).bind(storage::now()).execute(&app.db).await.context("无法预留线路")?;
    let row = sqlx::query(&format!(
        "{SELECT_LINES} WHERE l.panel_id=? AND l.name=? AND l.status IN ('pending','failed')"
    ))
    .bind(&panel.id)
    .bind(name)
    .fetch_one(&app.db)
    .await
    .context("无法取得线路")?;
    let rid = row.get::<String, _>("id");
    let uuid = row.get::<String, _>("uuid");
    let spec = json!({"name":name,"socks5":input.socks5.trim(),"connectHost":if panel.public_host.is_empty(){None}else{Some(&panel.public_host)},"expiryTime":row.get::<i64,_>("expiry"),"quotaBytes":row.get::<Option<i64>,_>("quota_bytes").unwrap_or(0)});
    sqlx::query("UPDATE lines SET status='pending',error=NULL,updated=? WHERE id=?")
        .bind(storage::now())
        .bind(&rid)
        .execute(&app.db)
        .await
        .context("无法保存任务")?;
    let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
    match api.provision_line(&spec, &uuid).await {
        Ok(result) => {
            let link = result["link"].as_str().context("面板未返回分享链接")?;
            let inbound = result["inboundId"].as_i64().context("面板未返回入站 ID")?;
            let end = result["expiryTime"]
                .as_i64()
                .context("面板未返回客户端到期时间")?;
            let quota = result["quotaBytes"]
                .as_i64()
                .context("面板未返回客户端流量上限")?;
            sqlx::query("UPDATE lines SET status='ready',link=?,inbound_id=?,expiry=?,quota_bytes=?,error=NULL,updated=? WHERE id=?")
                .bind(link).bind(inbound).bind(end).bind(quota).bind(storage::now()).bind(&rid).execute(&app.db).await.context("保存开通结果失败")?;
            Ok(line(&by_id(&app.db, &rid).await?))
        }
        Err(err) => {
            let msg = format!("{err:#}").replace(input.socks5.trim(), "[SOCKS5]");
            sqlx::query("UPDATE lines SET status='failed',error=?,updated=? WHERE id=?")
                .bind(&msg)
                .bind(storage::now())
                .bind(&rid)
                .execute(&app.db)
                .await
                .context("保存失败结果时出错")?;
            Err(ApiError(StatusCode::BAD_GATEWAY, msg))
        }
    }
}

#[derive(Deserialize)]
pub struct Term {
    months: u32,
}

#[derive(Deserialize)]
pub struct RepairReality {
    #[serde(default)]
    target: String,
}

pub async fn test_outbound(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let row = by_id(&app.db, &id).await?;
    let panel = app.panel(row.get("panel_id")).await?;
    let api = panel.api(&app.client)?;
    let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
    let delay = api
        .test_socks_outbound(row.get("client_email"), crate::upstream::ProbeMode::Http)
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(json!({"delay":delay})))
}

pub async fn repair_reality(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(input): Json<RepairReality>,
) -> ApiResult<Json<Value>> {
    let target = match input.target.trim() {
        "" => crate::upstream::default_reality_target(),
        value => value.to_owned(),
    };
    // Complete the link update even if the browser disconnects after the panel write.
    tokio::spawn(async move {
        let panel_id = by_id(&app.db, &id).await?.get::<String, _>("panel_id");
        let _guard = app.lock(&panel_id).await;
        let row = by_id(&app.db, &id).await?;
        if row.get::<String, _>("status") != "ready" {
            return Err(ApiError::bad("请先完成线路开通"));
        }
        let inbound = row
            .get::<Option<i64>, _>("inbound_id")
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| ApiError::bad("线路缺少入站 ID"))?;
        let panel = app.panel(&panel_id).await?;
        let host = crate::sync::sharing_host(&panel)?;
        let api = panel.api(&app.client)?;
        let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
        let (link, count) = api
            .repair_line_reality(
                inbound,
                row.get("client_email"),
                row.get("uuid"),
                &host,
                &target,
            )
            .await
            .map_err(ApiError::upstream)?;
        sqlx::query("UPDATE lines SET link=?,updated=? WHERE id=?")
            .bind(link)
            .bind(storage::now())
            .bind(&id)
            .execute(&app.db)
            .await
            .context("保存新的分享链接失败")?;
        Ok(Json(
            json!({"line":line(&by_id(&app.db,&id).await?),"target":target,"sniCount":count}),
        ))
    })
    .await
    .context("修复 Reality 任务中断")?
}

pub async fn repair_user_routes(
    State(app): State<Shared>,
    Path(panel_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let _guard = app.lock(&panel_id).await;
    let panel = app.panel(&panel_id).await?;
    // Old databases store both synced and locally provisioned lines. The panel
    // adapter also verifies the live binding and the entire legacy rule.
    let candidates = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT client_email, uuid, inbound_id FROM lines
         WHERE panel_id=? AND status='ready' AND inbound_id IS NOT NULL
           AND name=client_email AND name=inbound_name AND shared_client=0",
    )
    .bind(&panel_id)
    .fetch_all(&app.db)
    .await
    .context("无法读取当前面板的线路记录")?;
    let api = panel.api(&app.client)?;
    let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
    let report = api
        .repair_user_routes(&candidates)
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(report))
}

pub async fn renew(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(term): Json<Term>,
) -> ApiResult<Json<Value>> {
    let panel_id = by_id(&app.db, &id).await?.get::<String, _>("panel_id");
    let _guard = app.lock(&panel_id).await;
    let row = by_id(&app.db, &id).await?;
    if row.get::<String, _>("status") != "ready" {
        return Err(ApiError::bad("请先完成线路开通"));
    }
    let panel = app.panel(&panel_id).await?;
    let api = panel.api(&app.client)?;
    let name = row.get::<String, _>("client_email");
    let uuid = row.get::<String, _>("uuid");
    let inbound = row.get::<i64, _>("inbound_id") as u64;
    let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
    let clients = api.clients().await.map_err(ApiError::upstream)?;
    let client = clients
        .iter()
        .find(|c| c["uuid"] == uuid && c["email"] == name)
        .ok_or_else(|| ApiError::bad("原客户端已被移除或替换，请检查面板"))?;
    let current = client["expiryTime"]
        .as_i64()
        .ok_or_else(|| ApiError::bad("无法取得客户端当前到期时间"))?;
    let requested = expiry(current.max(storage::now()), term.months)?;
    let target = row
        .get::<Option<i64>, _>("renewal_target")
        .unwrap_or(requested);
    sqlx::query("UPDATE lines SET renewal_target=? WHERE panel_id=? AND uuid=? AND client_email=? AND status='ready'")
        .bind(target)
        .bind(&panel_id).bind(&uuid).bind(&name)
        .execute(&app.db)
        .await
        .context("保存续期任务失败")?;
    api.renew_line(inbound, &name, &uuid, target)
        .await
        .map_err(ApiError::upstream)?;
    sqlx::query("UPDATE lines SET expiry=?,renewal_target=NULL,updated=? WHERE panel_id=? AND uuid=? AND client_email=? AND status='ready'")
        .bind(target)
        .bind(storage::now())
        .bind(&panel_id).bind(&uuid).bind(&name)
        .execute(&app.db)
        .await
        .context("保存到期时间失败")?;
    Ok(Json(json!({"line":line(&by_id(&app.db,&id).await?)})))
}

pub async fn list(State(app): State<Shared>) -> ApiResult<Json<Value>> {
    let rows = sqlx::query(&format!("{SELECT_LINES} ORDER BY l.rowid DESC"))
        .fetch_all(&app.db)
        .await
        .context("无法读取线路列表")?;
    let mut output: Vec<_> = rows.iter().map(line).collect();
    let mut indexes = HashMap::<String, Vec<usize>>::new();
    for (i, row) in rows.iter().enumerate() {
        if row.get::<String, _>("status") == "ready" {
            indexes.entry(row.get("panel_id")).or_default().push(i);
        }
    }
    let mut errors = Vec::new();
    let stats: Vec<_> = stream::iter(
        storage::panels(&app.db)
            .await?
            .into_iter()
            .filter(|p| indexes.contains_key(&p.id)),
    )
    .map(|p| {
        let app = &app;
        async move {
            let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
            let data = match p.api(&app.client) {
                Ok(api) => {
                    let (clients, online, inbounds) =
                        tokio::join!(api.clients(), api.online_clients(), api.inbounds());
                    let inbound_states = inbounds.map(|rows| {
                        rows.iter()
                            .filter_map(|row| Some((row["id"].as_i64()?, row["enable"].as_bool()?)))
                            .collect::<HashMap<_, _>>()
                    });
                    Ok((clients, online, inbound_states))
                }
                Err(e) => Err(e),
            };
            (p.id, data)
        }
    })
    .buffer_unordered(6)
    .collect()
    .await;
    let mut client_counts = HashMap::new();
    for (pid, data) in stats {
        let count = data
            .as_ref()
            .ok()
            .and_then(|(clients, _, _)| clients.as_ref().ok())
            .map(Vec::len);
        client_counts.insert(pid.clone(), count);
        let mut error =
            |e: anyhow::Error| errors.push(json!({"panelId":pid,"message":format!("{e:#}")}));
        let (clients, online, inbound_states) = match data {
            Ok(v) => v,
            Err(e) => {
                error(e);
                continue;
            }
        };
        let online = match online {
            Ok(items) => Some(items.into_iter().collect::<HashSet<_>>()),
            Err(e) => {
                error(e);
                None
            }
        };
        let clients = match clients {
            Ok(v) => v,
            Err(e) => {
                error(e);
                continue;
            }
        };
        let clients: HashMap<_, _> = clients
            .iter()
            .filter_map(|c| Some(((c["uuid"].as_str()?, c["email"].as_str()?), c)))
            .collect();
        let inbound_states = inbound_states.unwrap_or_else(|err| {
            error(err.context("读取入站启用状态失败"));
            HashMap::new()
        });
        for &i in &indexes[&pid] {
            let row = &rows[i];
            let dst = &mut output[i];
            let uuid: String = row.get("uuid");
            let name: String = row.get("client_email");
            let inbound_id = row.get::<Option<i64>, _>("inbound_id");
            if let Some(client) = clients.get(&(uuid.as_str(), name.as_str())).filter(|c| {
                c["inboundIds"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&json!(inbound_id)))
            }) {
                let inbound_enabled = inbound_id.and_then(|id| inbound_states.get(&id).copied());
                match (client["enable"].as_bool(), inbound_enabled) {
                    (Some(false), _) | (_, Some(false)) => dst["enabled"] = json!(false),
                    (Some(true), Some(true)) => dst["enabled"] = json!(true),
                    _ => {}
                }
                dst["online"] = json!(online.as_ref().map(|o| o.contains(&name)));
                dst["up"] = client["traffic"]["up"].clone();
                dst["down"] = client["traffic"]["down"].clone();
                if client["expiryTime"].is_i64() {
                    dst["expiryTime"] = client["expiryTime"].clone();
                }
                if client["totalGB"].as_i64().is_some_and(|n| n >= 0) {
                    dst["quotaBytes"] = client["totalGB"].clone();
                }
            } else {
                dst["error"] = json!("原客户端已移除或改名，请检查目标面板");
            }
        }
    }
    Ok(Json(
        json!({"lines":output,"errors":errors,"clientCounts":client_counts}),
    ))
}

#[derive(Deserialize)]
pub struct Qr {
    link: String,
}
pub async fn qr(
    Json(input): Json<Qr>,
) -> ApiResult<([(header::HeaderName, &'static str); 1], String)> {
    if !input.link.starts_with("vless://") || input.link.len() > 4096 {
        return Err(ApiError::bad("需要有效的 VLESS 分享链接"));
    }
    let code = QrCode::new(input.link.as_bytes())
        .map_err(|_| ApiError::bad("分享链接过长，无法生成二维码"))?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(600, 600)
        .quiet_zone(true)
        .build();
    Ok(([(header::CONTENT_TYPE, "image/svg+xml")], svg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_and_batch_creation_default_to_unlimited() {
        let entry = json!({"name":"东京-01","socks5":"127.0.0.1:1080"});
        let single: CreateLine = serde_json::from_value(json!({
            "panelId":"panel","name":entry["name"],"socks5":entry["socks5"]
        }))
        .unwrap();
        let batch: CreateBatch = serde_json::from_value(json!({
            "panelId":"panel","lines":[entry]
        }))
        .unwrap();
        for limits in [single.limits, batch.limits] {
            assert_eq!(limits.resolve().ok(), Some((0, 0)));
        }
        let limits: CreationLimits = serde_json::from_value(json!({"months":1})).unwrap();
        assert!(limits
            .resolve()
            .ok()
            .is_some_and(|(end, _)| end > storage::now()));
        let date = 1_890_046_800_000_i64;
        let quota = 123_456_789_012_i64;
        let single: CreateLine = serde_json::from_value(
            json!({"panelId":"p","name":"a","socks5":"s","quotaBytes":quota,"expiryTime":date}),
        )
        .unwrap();
        let batch: CreateBatch = serde_json::from_value(
            json!({"panelId":"p","lines":[entry],"quotaBytes":quota,"expiryTime":date}),
        )
        .unwrap();
        for limits in [single.limits, batch.limits] {
            assert_eq!(limits.resolve().ok(), Some((date, quota)));
        }
        for invalid in [
            json!({"months":4}),
            json!({"months":3,"expiryTime":date}),
            json!({"expiryTime":-1}),
            json!({"quotaBytes":-2}),
        ] {
            assert!(serde_json::from_value::<CreationLimits>(invalid)
                .unwrap()
                .resolve()
                .is_err());
        }
    }

    #[test]
    fn terms_match_calendar_months_including_the_end_of_february() {
        let millis = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis();
        let base = millis("2028-01-31T08:15:00Z");
        assert_eq!(expiry(base, 1).ok(), Some(millis("2028-02-29T08:15:00Z")));
        assert_eq!(expiry(base, 2).ok(), Some(millis("2028-03-31T08:15:00Z")));
        assert_eq!(expiry(base, 3).ok(), Some(millis("2028-04-30T08:15:00Z")));
        assert_eq!(expiry(base, 6).ok(), Some(millis("2028-07-31T08:15:00Z")));
        assert_eq!(expiry(base, 12).ok(), Some(millis("2029-01-31T08:15:00Z")));
        assert!(expiry(base, 4).is_err());
    }

    #[test]
    fn replacing_a_share_host_preserves_the_port_credentials_and_name() {
        let link = "vless://user@example.com:20719?security=reality#%E5%8F%B0%E6%B9%BE";
        let changed = with_host(link, "[2001:db8::1]").unwrap();
        assert_eq!(changed, link.replace("example.com", "[2001:db8::1]"));
        assert!(public_host("example.com:8443").is_err());
        assert!(public_host("[[::1]]").is_err());
    }
}
