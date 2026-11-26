use crate::{storage, sync, ApiError, ApiResult, Shared};
use anyhow::{ensure, Context, Result};
use axum::{
    extract::{Path, State},
    Json,
};
use futures_util::{stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::{HashMap, HashSet};

mod edit;
mod export;
mod qr_png;
pub use edit::{extend, notes};
pub use export::export;

pub async fn links(
    State(app): State<Shared>,
    Path(panel_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let panel = app.panel(&panel_id).await?;
    let api = panel.api(&app.client)?;
    let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
    let (inbounds, mut clients) =
        tokio::try_join!(api.inbounds(), api.clients()).map_err(ApiError::upstream)?;
    let (links, warnings) = sync::client_links(&inbounds, &clients, &sync::sharing_host(&panel)?);
    for client in &mut clients {
        if let Some(obj) = client.as_object_mut() {
            let note = obj
                .get("comment")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            obj.insert("note".to_owned(), json!(note));
        }
    }
    let inbounds:Vec<_>=inbounds.iter().map(|ib|json!({"id":ib["id"],"remark":ib["remark"],"tag":ib["tag"],"protocol":ib["protocol"],"port":ib["port"],"enable":ib["enable"]})).collect();
    Ok(Json(
        json!({"clients":clients,"inbounds":inbounds,"links":links,"warnings":warnings}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Target {
    panel_id: String,
    client_id: i64,
    email: String,
    #[serde(default)]
    uuid: String,
    expiry_time: Option<i64>,
    updated_at: Option<i64>,
    note: Option<String>,
}

#[derive(Deserialize)]
pub struct DeleteClients {
    targets: Vec<Target>,
}

pub async fn pending(State(app): State<Shared>) -> ApiResult<Json<Value>> {
    let rows=sqlx::query("SELECT d.panel_id,p.name,d.client_id,d.email,d.uuid,d.error FROM client_deletions d JOIN panels p ON p.id=d.panel_id ORDER BY d.rowid")
        .fetch_all(&app.db).await.context("读取待清理客户端失败")?;
    let pending: Vec<_> = rows
        .iter()
        .map(|r| {
            json!({
                "panelId":r.get::<String,_>("panel_id"),"panelName":r.get::<String,_>("name"),
                "clientId":r.get::<i64,_>("client_id"),"email":r.get::<String,_>("email"),
                "uuid":r.get::<String,_>("uuid"),"error":r.get::<String,_>("error")
            })
        })
        .collect();
    Ok(Json(json!({"pending":pending})))
}

async fn delete_target(
    app: &Shared,
    target: &Target,
    api: &crate::upstream::PanelApi,
) -> Result<String> {
    let saved = sqlx::query(
        "SELECT email,uuid,plan FROM client_deletions WHERE panel_id=? AND client_id=?",
    )
    .bind(&target.panel_id)
    .bind(target.client_id)
    .fetch_optional(&app.db)
    .await?;
    let plan = if let Some(row) = saved {
        ensure!(
            row.get::<&str, _>("email") == target.email
                && row.get::<&str, _>("uuid") == target.uuid,
            "此编号已有一项待清理任务，请核对原客户端后重试"
        );
        serde_json::from_str(row.get("plan")).context("读取待清理配置失败")?
    } else {
        let plan = api
            .plan_client_delete(&target.email, target.client_id, &target.uuid)
            .await?;
        sqlx::query("INSERT INTO client_deletions(panel_id,client_id,email,uuid,plan,error) VALUES(?,?,?,?,?,?)")
            .bind(&target.panel_id).bind(target.client_id).bind(&target.email).bind(&target.uuid)
            .bind(serde_json::to_string(&plan)?).bind("尚未收到最终结果").execute(&app.db).await.context("保存客户端清理任务失败")?;
        plan
    };
    // Retain the plan until the upstream cleanup AND the local inventory update have completed.
    let message = match api.delete_client_cascade(&plan).await {
        Ok(message) => message,
        Err(err) => {
            sqlx::query("UPDATE client_deletions SET error=? WHERE panel_id=? AND client_id=?")
                .bind(format!("{err:#}"))
                .bind(&target.panel_id)
                .bind(target.client_id)
                .execute(&app.db)
                .await?;
            return Err(err);
        }
    };
    let mut tx = app.db.begin().await?;
    sqlx::query("DELETE FROM lines WHERE panel_id=? AND ((client_record=? AND uuid=?) OR (client_record IS NULL AND uuid=? AND client_email=?))")
        .bind(&target.panel_id).bind(target.client_id).bind(&target.uuid).bind(&target.uuid).bind(&target.email)
        .execute(&mut *tx).await?;
    sqlx::query("DELETE FROM client_deletions WHERE panel_id=? AND client_id=?")
        .bind(&target.panel_id)
        .bind(target.client_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(message)
}

pub async fn delete(
    State(app): State<Shared>,
    Json(input): Json<DeleteClients>,
) -> ApiResult<Json<Value>> {
    execute(app, input.targets, Operation::Delete).await
}

enum Operation {
    Delete,
    Extend(edit::Term),
    Notes(String),
}

async fn execute(
    app: Shared,
    targets: Vec<Target>,
    operation: Operation,
) -> ApiResult<Json<Value>> {
    let mut unique = HashSet::new();
    if targets.is_empty()
        || targets.len() > 512
        || targets.iter().any(|t| {
            t.client_id <= 0
                || t.email.is_empty()
                || t.email.len() > 1024
                || t.uuid.len() > 256
                || t.panel_id.is_empty()
                || t.panel_id.len() > 100
                || !unique.insert((&t.panel_id, t.client_id))
        })
    {
        return Err(ApiError::bad("请选择 1 至 512 个不同的客户端"));
    }
    if matches!(operation, Operation::Extend(_)) && targets.iter().any(|t| t.expiry_time.is_none())
    {
        return Err(ApiError::bad(
            "续期须提供所选客户端的当前到期时间，请刷新列表",
        ));
    }
    let results = tokio::spawn(run_batch(app, targets, operation))
        .await
        .context("客户端操作中断")??;
    Ok(Json(json!({"results":results})))
}
async fn run_batch(app: Shared, targets: Vec<Target>, operation: Operation) -> Result<Vec<Value>> {
    let mut groups: HashMap<String, Vec<(usize, Target)>> = HashMap::new();
    for (i, target) in targets.into_iter().enumerate() {
        groups
            .entry(target.panel_id.clone())
            .or_default()
            .push((i, target));
    }
    let groups: Vec<_> = stream::iter(groups)
        .map(|(panel_id, targets)| {
            let app = app.clone();
            let operation = &operation;
            async move {
                let _guard = app.lock(&panel_id).await;
                let panel = app.panel(&panel_id).await;
                let name = panel
                    .as_ref()
                    .map(|p| p.name.as_str())
                    .unwrap_or("已移除的面板");
                let adapter = panel
                    .as_ref()
                    .map_err(|e| anyhow::anyhow!("{}", e.1))
                    .and_then(|p| p.api(&app.client));
                let mut rows = Vec::new();
                for (index, target) in targets {
                    let result = match &adapter {
                        Ok(api) => {
                            let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
                            match operation {
                                Operation::Delete => delete_target(&app, &target, api)
                                    .await
                                    .map(|msg| json!({"message":msg})),
                                Operation::Extend(term) => {
                                    edit::extend_target(&app.db, &target, api, *term).await
                                }
                                Operation::Notes(note) => {
                                    edit::note_target(&target, api, note).await
                                }
                            }
                        }
                        Err(err) => Err(anyhow::anyhow!("{err:#}")),
                    };
                    let ok = result.is_ok();
                    let mut row = result.unwrap_or_else(|err| {
                        json!({"message":format!("{err:#}"), "conflict":err.is::<edit::Conflict>()})
                    });
                    let message = row["message"].as_str().unwrap_or_default();
                    let outcome = storage::Outcome {
                        panel_id: panel_id.clone(),
                        name: name.to_owned(),
                        ok,
                        message: format!("{} · {message}", target.email),
                    };
                    row["panelId"] = json!(panel_id);
                    row["name"] = json!(name);
                    row["email"] = json!(target.email);
                    row["clientId"] = json!(target.client_id);
                    row["uuid"] = json!(target.uuid);
                    row["ok"] = json!(ok);
                    rows.push((index, row, outcome));
                }
                rows
            }
        })
        .buffer_unordered(6)
        .collect()
        .await;
    let mut rows: Vec<_> = groups.into_iter().flatten().collect();
    rows.sort_by_key(|(i, _, _)| *i);
    let (results, outcomes): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .map(|(_, result, outcome)| (result, outcome))
        .unzip();
    let action = match operation {
        Operation::Delete => "client.delete",
        Operation::Extend(_) => "client.extend",
        Operation::Notes(_) => "client.notes",
    };
    storage::log_start(&app.db, action, &outcomes).await?;
    Ok(results)
}
