use crate::{
    storage::{self, Outcome, Panel},
    ApiError, ApiResult, Shared,
};
use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::{stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    io::{Seek, Write},
};
use tokio_util::io::ReaderStream;
use zip::{write::SimpleFileOptions, ZipWriter};

#[derive(Deserialize)]
pub(crate) struct Batch {
    action: String,
    targets: Vec<Target>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Target {
    panel_id: String,
    payload: Value,
}

fn target_label(action: &str, payload: &Value) -> String {
    let label = match action {
        "inbound.create" => format!(
            "端口 {}",
            payload
                .get("port")
                .and_then(Value::as_u64)
                .unwrap_or_default()
        ),
        "inbound.expiry" => format!(
            "入站 #{}",
            payload
                .get("inboundId")
                .and_then(Value::as_u64)
                .unwrap_or_default()
        ),
        "client.create" | "client.expiry" => {
            payload["email"].as_str().unwrap_or("客户端").to_owned()
        }
        "outbound.upsert" => payload["outbound"]["tag"]
            .as_str()
            .unwrap_or("出站")
            .to_owned(),
        _ => payload["rule"]["ruleTag"]
            .as_str()
            .or_else(|| payload["rule"]["outboundTag"].as_str())
            .unwrap_or("路由规则")
            .to_owned(),
    };
    label.chars().take(120).collect()
}

fn pending(id: &str, panels: &HashMap<String, Panel>) -> Outcome {
    Outcome {
        panel_id: id.into(),
        name: panels
            .get(id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "已移除的面板".into()),
        ok: false,
        message: "尚未收到最终结果，请核对面板后再重试".into(),
    }
}

async fn panel_map(app: &Shared) -> Result<HashMap<String, Panel>> {
    Ok(storage::panels(&app.db)
        .await?
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect())
}

pub(crate) async fn execute(
    State(app): State<Shared>,
    Json(batch): Json<Batch>,
) -> ApiResult<Json<Value>> {
    if !matches!(
        batch.action.as_str(),
        "inbound.create"
            | "inbound.expiry"
            | "client.create"
            | "client.expiry"
            | "outbound.upsert"
            | "route.upsert"
    ) {
        return Err(ApiError::bad("不支持的批量操作"));
    }
    if batch.targets.is_empty()
        || batch.targets.len() > 512
        || batch
            .targets
            .iter()
            .any(|t| !t.payload.is_object() || t.panel_id.len() > 100)
    {
        return Err(ApiError::bad("请选择 1 至 512 个目标，并提供对应配置"));
    }
    // A browser navigation must not cancel an already dispatched mutation.
    let result = tokio::spawn(run_batch(app, batch))
        .await
        .context("批量任务中断")??;
    Ok(Json(result))
}

async fn run_batch(app: Shared, batch: Batch) -> Result<Value> {
    let panels = panel_map(&app).await?;
    let mut outcomes: Vec<_> = batch
        .targets
        .iter()
        .map(|t| pending(&t.panel_id, &panels))
        .collect();
    let job = storage::log_start(&app.db, &batch.action, &outcomes).await?;
    let mut groups: HashMap<String, Vec<(usize, Value)>> = HashMap::new();
    for (i, target) in batch.targets.into_iter().enumerate() {
        groups
            .entry(target.panel_id)
            .or_default()
            .push((i, target.payload));
    }
    let action = &batch.action;
    let changes: Vec<Vec<(usize, Result<String>)>> = stream::iter(groups)
        .map(|(id, targets)| {
            let app = &app;
            let job = &job;
            async move {
                let _guard = app.lock(&id).await;
                let api = match app.panel(&id).await {
                    Ok(p) => p.api(&app.client),
                    Err(e) => Err(anyhow::anyhow!(e.1)),
                };
                let mut results = Vec::with_capacity(targets.len());
                for (i, payload) in targets {
                    let result = match &api {
                        Ok(adapter) => {
                            let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
                            adapter.apply(action, &payload).await
                        }
                        Err(e) => Err(anyhow::anyhow!("{e:#}")),
                    }
                    .map_err(|e| anyhow::anyhow!("{} · {e:#}", target_label(action, &payload)));
                    let (ok, msg) = match &result {
                        Ok(s) => (true, s.clone()),
                        Err(e) => (false, format!("{e:#}")),
                    };
                    if let Err(err) = sqlx::query(
                        "UPDATE history SET results=json_set(results,?,json(?),?,?) WHERE id=?",
                    )
                    .bind(format!("$[{i}].ok"))
                    .bind(if ok { "true" } else { "false" })
                    .bind(format!("$[{i}].message"))
                    .bind(msg)
                    .bind(job)
                    .execute(&app.db)
                    .await
                    {
                        eprintln!("写入单项目标结果失败: {err}");
                    }
                    results.push((i, result));
                }
                results
            }
        })
        .buffer_unordered(6)
        .collect()
        .await;
    for (i, result) in changes.into_iter().flatten() {
        outcomes[i].ok = result.is_ok();
        outcomes[i].message = result.unwrap_or_else(|e| format!("{e:#}"));
    }
    storage::log_finish(&app.db, &job, &outcomes).await?;
    Ok(json!({"id":job,"results":outcomes}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Backup {
    panel_ids: Vec<String>,
    #[serde(default)]
    mode: BackupMode,
    date: Option<String>,
}

#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum BackupMode {
    Single,
    #[default]
    Archive,
}

fn backup_day(date: Option<&str>) -> ApiResult<chrono::NaiveDate> {
    match date {
        None => Ok(chrono::Local::now().date_naive()),
        Some(s) if s.len() == 10 => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| ApiError::bad("备份日期格式应为 YYYY-MM-DD")),
        _ => Err(ApiError::bad("备份日期格式应为 YYYY-MM-DD")),
    }
}

fn file_host(url: &str) -> String {
    let parsed = reqwest::Url::parse(url).expect("stored panel URL");
    let host = parsed
        .host_str()
        .unwrap_or("panel")
        .trim_matches(['[', ']', '.']);
    // ASCII-only header and ZIP components; URL parsing already converts international domains to IDNA.
    let host: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_') {
                c
            } else {
                '-'
            }
        })
        .take(180)
        .collect();
    let first = host.split('.').next().unwrap_or("").to_ascii_uppercase();
    let device = matches!(first.as_str(), "CON" | "NUL" | "AUX" | "PRN")
        || (first.len() == 4
            && (first.starts_with("COM") || first.starts_with("LPT"))
            && first.as_bytes()[3].is_ascii_digit());
    if host.is_empty() {
        "panel".to_owned()
    } else if device {
        format!("_{host}")
    } else {
        host
    }
}

fn download(
    file: std::fs::File,
    size: u64,
    name: &str,
    mime: &str,
    ok: usize,
    failed: usize,
) -> ApiResult<Response> {
    let body = Body::from_stream(ReaderStream::new(tokio::fs::File::from_std(file)));
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, size)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        )
        .header("x-backup-success", ok)
        .header("x-backup-failed", failed)
        .body(body)
        .context("生成下载响应失败")?)
}

pub(crate) async fn backup(
    State(app): State<Shared>,
    Json(req): Json<Backup>,
) -> ApiResult<Response> {
    let mut unique = HashSet::new();
    let date = backup_day(req.date.as_deref())?;
    let single = req.mode == BackupMode::Single;
    if single && req.panel_ids.len() != 1 {
        return Err(ApiError::bad("单面板下载需要选择一个面板"));
    }
    if req.panel_ids.is_empty()
        || req.panel_ids.len() > 128
        || req
            .panel_ids
            .iter()
            .any(|s| s.len() > 100 || !unique.insert(s))
    {
        return Err(ApiError::bad("请选择 1 至 128 个不同的面板"));
    }
    let _slot = app.backup_slot.acquire().await.expect("open semaphore");
    let panels = panel_map(&app).await?;
    let mut results: Vec<_> = req
        .panel_ids
        .iter()
        .map(|id| pending(id, &panels))
        .collect();
    let job = storage::log_start(&app.db, "backup", &results).await?;
    let file = tempfile::tempfile().context("无法创建临时备份文件")?;
    let mut zip = ZipWriter::new(file);
    let mut files = Vec::new();
    let mut used_names = HashSet::new();
    let mut archive_hosts = Vec::new();
    for result in &mut results {
        let fetch: Result<_> = async {
            let panel = panels.get(&result.panel_id).context("该连接已移除")?;
            let _guard = app.lock(&panel.id).await;
            let _slot = app.upstream_slots.acquire().await?;
            let backup = panel.api(&app.client)?.backup().await?;
            Ok((backup, file_host(&panel.url)))
        }
        .await;
        match fetch {
            Err(e) => result.message = format!("{e:#}"),
            Ok((mut backup, host)) => {
                let stem = format!("{host}-{date}");
                let ext = backup.extension;
                let mut name = format!("{stem}.{ext}");
                if single {
                    result.ok = true;
                    result.message = "已导出面板数据库".into();
                    storage::log_finish(&app.db, &job, &results).await?;
                    return download(
                        backup.file,
                        backup.size,
                        &name,
                        "application/octet-stream",
                        1,
                        0,
                    );
                }
                if !archive_hosts.contains(&host) {
                    archive_hosts.push(host);
                }
                let mut counter = 2;
                while !used_names.insert(name.clone()) {
                    name = format!("{stem}-{counter}.{ext}");
                    counter += 1;
                }
                files.push(json!({"panelId":result.panel_id,"name":result.name,"file":name,"bytes":backup.size}));
                zip = tokio::task::spawn_blocking(move || -> Result<_> {
                    zip.start_file(
                        name,
                        SimpleFileOptions::default()
                            .large_file(backup.size >= u32::MAX as u64)
                            .compression_method(zip::CompressionMethod::Stored),
                    )?;
                    std::io::copy(&mut backup.file, &mut zip)?;
                    Ok(zip)
                })
                .await
                .context("备份归档中断")??;
                result.ok = true;
                result.message = "已导出面板数据库".into();
            }
        }
    }
    if files.is_empty() {
        storage::log_finish(&app.db, &job, &results).await?;
        return Ok((
            StatusCode::BAD_GATEWAY,
            Json(json!({"error":"所有面板备份均失败，请查看操作记录","results":results})),
        )
            .into_response());
    }
    let manifest = json!({"version":1,"createdAt":storage::now(),"files":files,"results":results});
    zip.start_file(
        "manifest.json",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .context("写入备份清单失败")?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest).context("生成备份清单失败")?)
        .context("写入备份清单失败")?;
    let mut file = zip.finish().context("完成归档失败")?;
    let size = file.metadata().context("读取归档大小失败")?.len();
    file.rewind().context("读取归档失败")?;
    storage::log_finish(&app.db, &job, &results).await?;
    let mut prefix = archive_hosts.join("+");
    if prefix.len() > 180 {
        prefix = format!("{}+{}-more", archive_hosts[0], archive_hosts.len() - 1);
    }
    download(
        file,
        size,
        &format!("{prefix}-{date}.zip"),
        "application/zip",
        files.len(),
        results.len() - files.len(),
    )
}
