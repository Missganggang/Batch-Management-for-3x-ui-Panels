use super::{execute, Operation, Target};
use crate::{storage, upstream::PanelApi, ApiError, ApiResult, Shared};
use anyhow::{ensure, Context, Result};
use axum::{extract::State, Json};
use chrono::{DateTime, Months};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::SqlitePool;

#[derive(Debug)]
pub(super) struct Conflict(&'static str);

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Conflict {}

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
pub struct Extension {
    targets: Vec<Target>,
    months: Option<u32>,
    days: Option<i64>,
}

#[derive(Clone, Copy)]
pub(super) enum Term {
    Months(u32),
    Days(i64),
}

impl Term {
    fn fields(self) -> (&'static str, i64) {
        match self {
            Self::Months(n) => ("months", n.into()),
            Self::Days(n) => ("days", n),
        }
    }

    fn extend(self, base: i64, now: i64) -> Result<i64> {
        const MAX_DATE: i64 = 8_640_000_000_000_000;
        let start = base.max(now);
        let delta = match self {
            Self::Months(n) => {
                let date =
                    DateTime::from_timestamp_millis(start).context("客户端到期时间超出范围")?;
                let end = date
                    .checked_add_months(Months::new(n))
                    .context("续期后的时间超出范围")?;
                (end - date).num_milliseconds()
            }
            Self::Days(n) => n
                .checked_mul(86_400_000)
                .context("续期天数超出可表示范围")?,
        };
        if base < 0 {
            // A negative 3x-ui expiry is a duration that begins on first use.
            let deferred = base.checked_sub(delta).context("首次使用时长超出范围")?;
            ensure!(
                now.checked_sub(deferred).is_some_and(|end| end <= MAX_DATE),
                "续期后的时间超出可表示范围"
            );
            Ok(deferred)
        } else {
            start
                .checked_add(delta)
                .filter(|end| *end <= MAX_DATE)
                .context("续期后的时间超出可表示范围")
        }
    }
}

pub async fn extend(
    State(app): State<Shared>,
    Json(input): Json<Extension>,
) -> ApiResult<Json<Value>> {
    let term = match (input.months, input.days) {
        (Some(n @ (1 | 2 | 3 | 6 | 12)), None) => Term::Months(n),
        (None, Some(n)) if n > 0 => Term::Days(n),
        _ => return Err(ApiError::bad("请选择 1、2、3、6、12 个月或输入正整数天数")),
    };
    execute(app, input.targets, Operation::Extend(term)).await
}

#[derive(Deserialize)]
pub struct Note {
    targets: Vec<Target>,
    note: String,
}

pub async fn notes(State(app): State<Shared>, Json(input): Json<Note>) -> ApiResult<Json<Value>> {
    if input.note.chars().count() > 2048 {
        return Err(ApiError::bad("备注最多可输入 2048 个字符"));
    }
    execute(app, input.targets, Operation::Notes(input.note)).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quota {
    targets: Vec<Target>,
    quota_bytes: i64,
}

pub async fn quota(State(app): State<Shared>, Json(input): Json<Quota>) -> ApiResult<Json<Value>> {
    if input.quota_bytes < 0 {
        return Err(ApiError::bad("流量上限须为非负整数字节，0 表示不限"));
    }
    execute(app, input.targets, Operation::Quota(input.quota_bytes)).await
}

async fn record(api: &PanelApi, target: &Target) -> Result<(Value, i64)> {
    let mut details = api.client_details(&target.email).await?;
    let record = details["client"].take();
    ensure!(
        record["id"].as_i64() == Some(target.client_id)
            && record["email"] == target.email
            && record["uuid"].as_str() == Some(&target.uuid),
        "客户端身份已变化，请刷新列表后重试"
    );
    let expiry = record["expiryTime"]
        .as_i64()
        .context("面板未返回客户端到期时间")?;
    Ok((record, expiry))
}

pub(super) async fn quota_target(
    db: &SqlitePool,
    target: &Target,
    api: &PanelApi,
    bytes: i64,
) -> Result<Value> {
    let (mut client, expiry) = record(api, target).await?;
    let current = client["totalGB"]
        .as_i64()
        .context("面板未返回客户端流量上限")?;
    let mut message = "已设置客户端流量上限";
    if current != bytes {
        ensure!(
            target
                .quota_bytes
                .is_none_or(|expected| expected == current),
            Conflict("客户端流量上限已变化，请刷新列表后重试")
        );
        client["totalGB"] = json!(bytes);
        let reply = api
            .save_client_record(client, &target.email, expiry)
            .await?;
        let (saved, _) = record(api, target).await?;
        ensure!(
            saved["totalGB"].as_i64() == Some(bytes),
            "面板回读的额度与所设值不符，请核对客户端的入站绑定及当前额度"
        );
        if reply["nodePending"] == true {
            message = "已设置流量上限，等待节点同步";
        }
    }
    sqlx::query(
        "UPDATE lines SET quota_bytes=?,updated=? WHERE panel_id=? AND status='ready'
         AND uuid=? AND client_email=? AND (client_record=? OR client_record IS NULL)",
    )
    .bind(bytes)
    .bind(storage::now())
    .bind(&target.panel_id)
    .bind(&target.uuid)
    .bind(&target.email)
    .bind(target.client_id)
    .execute(db)
    .await?;
    Ok(json!({"message":message,"quotaBytes":bytes}))
}

pub(super) async fn reset_target(target: &Target, api: &PanelApi) -> Result<Value> {
    record(api, target).await?;
    api.reset_client_traffic(&target.email)
        .await
        .context("请刷新列表核对当前流量后再决定是否重新清零")?;
    Ok(json!({"message":"已重置客户端的已用流量"}))
}

pub(super) async fn note_target(target: &Target, api: &PanelApi, note: &str) -> Result<Value> {
    let (mut record, expiry) = record(api, target).await?;
    let current = record["comment"].as_str().context("面板未返回客户端备注")?;
    let mut message = "已保存客户端备注";
    if current != note {
        ensure!(
            target
                .note
                .as_deref()
                .is_none_or(|expected| expected == current),
            Conflict("备注已被修改，请刷新列表后重试")
        );
        record["comment"] = json!(note);
        let reply = api
            .save_client_record(record, &target.email, expiry)
            .await?;
        if reply["nodePending"] == true {
            message = "已保存客户端备注，等待节点同步";
        }
    }
    Ok(json!({"message":message,"note":note}))
}

pub(super) async fn extend_target(
    db: &SqlitePool,
    target: &Target,
    api: &PanelApi,
    term: Term,
) -> Result<Value> {
    let base = target.expiry_time.context("缺少客户端原到期时间")?;
    let (record, current) = record(api, target).await?;
    let (unit, amount) = term.fields();
    let version = target.updated_at.unwrap_or(-1);
    let saved = sqlx::query_as::<_, (i64, i64, bool)>(
        "SELECT rowid,target_expiry,applied FROM client_extensions
         WHERE panel_id=? AND client_id=? AND email=? AND uuid=? AND base=? AND updated_at=? AND unit=? AND amount=?",
    )
    .bind(&target.panel_id)
    .bind(target.client_id)
    .bind(&target.email)
    .bind(&target.uuid)
    .bind(base)
    .bind(version)
    .bind(unit)
    .bind(amount)
    .fetch_optional(db)
    .await?;
    let (intent_id, expiry, applied) = match saved {
        Some(intent) => intent,
        None => {
            ensure!(
                current == base
                    && target
                        .updated_at
                        .is_none_or(|v| record["updatedAt"].as_i64() == Some(v)),
                Conflict("客户端已变化，请刷新列表后重试")
            );
            let expiry = term.extend(base, storage::now())?;
            // Store the exact target before the network write. A lost response must not add time twice.
            let row = sqlx::query(
                "INSERT INTO client_extensions(panel_id,client_id,email,uuid,base,updated_at,unit,amount,target_expiry)
                 VALUES(?,?,?,?,?,?,?,?,?)",
            )
            .bind(&target.panel_id)
            .bind(target.client_id)
            .bind(&target.email)
            .bind(&target.uuid)
            .bind(base)
            .bind(version)
            .bind(unit)
            .bind(amount)
            .bind(expiry)
            .execute(db)
            .await?;
            (row.last_insert_rowid(), expiry, false)
        }
    };
    let mut message = "已延长客户端有效期";
    if current != expiry {
        ensure!(
            current == base
                && !applied
                && target
                    .updated_at
                    .is_none_or(|v| record["updatedAt"].as_i64() == Some(v)),
            "到期时间已变化，请刷新列表后重试"
        );
        let reply = api
            .save_client_record(record, &target.email, expiry)
            .await?;
        if reply["nodePending"] == true {
            message = "已延长客户端有效期，等待节点同步";
        }
    }
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE client_extensions SET applied=1 WHERE rowid=?")
        .bind(intent_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE lines SET expiry=?,renewal_target=NULL,updated=?
        WHERE panel_id=? AND status='ready' AND uuid=? AND client_email=?
          AND (client_record=? OR client_record IS NULL)",
    )
    .bind(expiry)
    .bind(storage::now())
    .bind(&target.panel_id)
    .bind(&target.uuid)
    .bind(&target.email)
    .bind(target.client_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"message":message,"expiryTime":expiry}))
}
