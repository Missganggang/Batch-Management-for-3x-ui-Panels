use crate::{storage, upstream, ApiError, ApiResult, Shared};
use anyhow::{bail, ensure, Context, Result};
use reqwest::Url;
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub async fn run(app: &Shared, panel_id: &str) -> ApiResult<Value> {
    let _guard = app.lock(panel_id).await;
    let panel = app.panel(panel_id).await?;
    let api = panel.api(&app.client)?;
    let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
    let (inbounds, clients, config) = tokio::try_join!(api.inbounds(), api.clients(), api.config())
        .map_err(ApiError::upstream)?;
    let snapshot =
        snapshot(&inbounds, &clients, &sharing_host(&panel)?).map_err(ApiError::upstream)?;
    let mut stats = save(&app.db, panel_id, snapshot).await?;
    stats["inbounds"] = json!(inbounds.len());
    stats["clients"] = json!(clients.len());
    stats["outbounds"] = json!(config["outbounds"].as_array().map_or(0, Vec::len));
    stats["rules"] = json!(config["routing"]["rules"].as_array().map_or(0, Vec::len));
    Ok(stats)
}

pub(crate) fn sharing_host(panel: &storage::Panel) -> Result<String> {
    if !panel.public_host.is_empty() {
        return Ok(panel.public_host.clone());
    }
    Ok(Url::parse(&panel.url)?
        .host_str()
        .context("面板缺少域名")?
        .to_owned())
}

// A partial share configuration on one inbound must not hide unrelated clients on the management page.
pub(crate) fn client_links(
    inbounds: &[Value],
    clients: &[Value],
    host: &str,
) -> (Vec<Value>, Vec<String>) {
    let mut links = Vec::new();
    let mut warnings = Vec::new();
    for raw in inbounds {
        if raw["protocol"] != "vless" {
            continue;
        }
        let Some(id) = raw["id"].as_i64() else {
            continue;
        };
        let inbound = match shareable_inbound(raw) {
            Ok(Some(v)) => v,
            Ok(None) => continue,
            Err(e) => {
                warnings.push(format!("入站 #{id}：{e}"));
                continue;
            }
        };
        for client in clients.iter().filter(|c| {
            c["inboundIds"]
                .as_array()
                .is_some_and(|v| v.iter().any(|n| n.as_i64() == Some(id)))
        }) {
            let (Some(uid), Some(email), Some(record)) = (
                client["uuid"].as_str(),
                client["email"].as_str(),
                client["id"].as_i64(),
            ) else {
                continue;
            };
            if uid.is_empty() {
                continue;
            }
            let (_, name) = binding_name(&inbound, email);
            match upstream::share_link(&inbound,&name,uid,host,client["flow"].as_str().unwrap_or_default()){
                Ok(link)=>links.push(json!({"clientId":record,"email":email,"uuid":uid,"inboundId":id,"name":name,"link":link})),
                Err(e)=>warnings.push(format!("{name}：{e}")),
            }
        }
    }
    (links, warnings)
}

struct Binding {
    inbound_id: i64,
    client_id: Option<i64>,
    uuid: String,
    email: String,
    inbound_name: String,
    name: String,
    link: String,
    expiry: i64,
    quota: Option<i64>,
    inbound_expiry: i64,
    enabled: bool,
    shared: bool,
}
struct Snapshot {
    bindings: Vec<Binding>,
    skipped: usize,
    warnings: Vec<String>,
}

fn native_json(value: &Value) -> Result<Value> {
    if let Some(s) = value.as_str() {
        serde_json::from_str(s).context("面板的入站设置无法解析")
    } else {
        Ok(value.clone())
    }
}

fn shareable_inbound(raw: &Value) -> Result<Option<Value>> {
    if raw["protocol"] != "vless" {
        return Ok(None);
    }
    let stream = native_json(&raw["streamSettings"])?;
    if stream["security"] != "reality" || !matches!(stream["network"].as_str(), Some("tcp" | "raw"))
    {
        return Ok(None);
    }
    let mut inbound = raw.clone();
    inbound["streamSettings"] = stream;
    inbound["settings"] = native_json(&raw["settings"])?;
    Ok(Some(inbound))
}
fn binding_name<'a>(ib: &'a Value, email: &'a str) -> (&'a str, String) {
    let remark = ib["remark"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| ib["tag"].as_str())
        .unwrap_or(email);
    (
        remark,
        if remark == email {
            email.to_string()
        } else {
            format!("{remark} · {email}")
        },
    )
}
fn snapshot(inbounds: &[Value], clients: &[Value], host: &str) -> Result<Snapshot> {
    let mut result = Snapshot {
        bindings: vec![],
        skipped: 0,
        warnings: vec![],
    };
    let mut supported = HashMap::new();
    let mut known = HashSet::new();
    for raw in inbounds {
        let id = raw["id"]
            .as_i64()
            .filter(|i| *i > 0)
            .context("面板返回的入站 ID 无效")?;
        ensure!(known.insert(id), "面板返回重复的入站 ID");
        let Some(inbound) = shareable_inbound(raw)? else {
            result.skipped += 1;
            continue;
        };
        supported.insert(id, inbound);
    }
    let mut seen = HashSet::new();
    for client in clients {
        let ids = client["inboundIds"]
            .as_array()
            .context("客户端列表缺少入站关联，已保留原库存")?;
        let email = client["email"].as_str().context("客户端缺少标识")?;
        for raw_id in ids {
            let id = raw_id.as_i64().context("客户端的入站关联无效")?;
            ensure!(known.contains(&id), "入站列表和客户端关联不同步，请重试");
            let Some(ib) = supported.get(&id) else {
                continue;
            };
            let uuid = client["uuid"]
                .as_str()
                .filter(|s| !s.is_empty())
                .context("VLESS 客户端缺少 UUID")?;
            let record = client["id"].as_i64().filter(|v| *v > 0);
            ensure!(seen.insert((id, email, uuid)), "面板返回重复的客户端关联");
            let (remark, name) = binding_name(ib, email);
            let flow = client["flow"].as_str().unwrap_or_default();
            let link = upstream::share_link(ib, &name, uuid, host, flow)
                .with_context(|| format!("线路「{name}」分享设置不完整，已保留原库存"))?;
            result.bindings.push(Binding {
                inbound_id: id,
                client_id: record,
                uuid: uuid.to_owned(),
                email: email.to_owned(),
                inbound_name: remark.to_owned(),
                name,
                link,
                expiry: client["expiryTime"]
                    .as_i64()
                    .context("客户端缺少到期时间")?,
                quota: client["totalGB"].as_i64().filter(|v| *v >= 0),
                inbound_expiry: ib["expiryTime"].as_i64().unwrap_or(0),
                enabled: client["enable"].as_bool().unwrap_or(true)
                    && ib["enable"].as_bool().unwrap_or(true),
                shared: ids.len() > 1,
            });
        }
    }
    // 3x-ui stores client bindings separately. If its two reads straddle an upstream
    // edit, preserve inventory and let the caller retry with a consistent snapshot.
    for (&id, ib) in &supported {
        let embedded = ib["settings"]["clients"].as_array();
        for entry in embedded.into_iter().flatten() {
            if let (Some(email), Some(uuid)) = (entry["email"].as_str(), entry["id"].as_str()) {
                ensure!(
                    seen.contains(&(id, email, uuid)),
                    "入站客户端与面板客户端列表不一致，请重新同步"
                );
            }
        }
    }
    if result.skipped > 0 {
        result.warnings.push(format!(
            "{} 个入站使用其他协议或传输方式，可在入站管理查看",
            result.skipped
        ));
    }
    Ok(result)
}

async fn save(db: &SqlitePool, panel_id: &str, snapshot: Snapshot) -> Result<Value> {
    let mut tx = db.begin().await?;
    let existing = sqlx::query("SELECT * FROM lines WHERE panel_id=?")
        .bind(panel_id)
        .fetch_all(&mut *tx)
        .await?;
    let mut stable = HashMap::new();
    let mut legacy = HashMap::new();
    let mut pending = HashSet::new();
    for (i, row) in existing.iter().enumerate() {
        if row.get::<String, _>("status") != "ready" {
            pending.insert(row.get::<String, _>("uuid"));
            continue;
        }
        let Some(id) = row.get::<Option<i64>, _>("inbound_id") else {
            continue;
        };
        if let Some(record) = row.get::<Option<i64>, _>("client_record") {
            stable.insert((id, record), i);
        } else {
            legacy.insert(
                (
                    id,
                    row.get::<String, _>("uuid"),
                    row.get::<String, _>("client_email"),
                ),
                i,
            );
        }
    }
    let (mut added, mut updated, mut removed, mut unchanged, mut skipped) =
        (0, 0, 0, 0, snapshot.skipped);
    let mut keep = HashSet::new();
    for b in snapshot.bindings {
        if pending.contains(&b.uuid) {
            skipped += 1;
            continue;
        }
        let key = (b.inbound_id, b.uuid.clone(), b.email.clone());
        let found = b
            .client_id
            .and_then(|cid| stable.get(&(b.inbound_id, cid)))
            .or_else(|| legacy.get(&key))
            .copied();
        if let Some(i) = found {
            if !keep.insert(i) {
                bail!("客户端绑定不唯一，已保留原库存");
            }
            let row = &existing[i];
            let id: String = row.get("id");
            let q=sqlx::query("UPDATE lines SET name=?1,uuid=?2,client_email=?3,inbound_name=?4,client_record=?5,expiry=?6,inbound_expiry=?7,enabled=?8,shared_client=?9,link=?10,updated=?11,quota_bytes=?13,
                renewal_target=CASE WHEN uuid=?2 AND client_email=?3 THEN renewal_target ELSE NULL END
                WHERE id=?12 AND (name,uuid,client_email,inbound_name,client_record,expiry,inbound_expiry,enabled,shared_client,link,quota_bytes) IS NOT (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?13)")
                .bind(b.name).bind(b.uuid).bind(b.email).bind(b.inbound_name).bind(b.client_id).bind(b.expiry).bind(b.inbound_expiry)
                .bind(b.enabled).bind(b.shared).bind(b.link).bind(storage::now()).bind(id).bind(b.quota).execute(&mut *tx).await?;
            if q.rows_affected() == 1 {
                updated += 1;
            } else {
                unchanged += 1;
            }
        } else {
            sqlx::query("INSERT INTO lines(id,panel_id,name,uuid,status,expiry,inbound_id,link,updated,client_email,inbound_name,client_record,shared_client,enabled,inbound_expiry,quota_bytes)
                VALUES(?,?,?,?,'ready',?,?,?,?,?,?,?,?,?,?,?)")
                .bind(Uuid::new_v4().to_string()).bind(panel_id).bind(b.name).bind(b.uuid).bind(b.expiry).bind(b.inbound_id).bind(b.link)
                .bind(storage::now()).bind(b.email).bind(b.inbound_name).bind(b.client_id).bind(b.shared).bind(b.enabled).bind(b.inbound_expiry).bind(b.quota)
                .execute(&mut *tx).await?;
            added += 1;
        }
    }
    for (i, row) in existing.iter().enumerate() {
        if row.get::<String, _>("status") == "ready" && !keep.contains(&i) {
            sqlx::query("DELETE FROM lines WHERE id=?")
                .bind(row.get::<String, _>("id"))
                .execute(&mut *tx)
                .await?;
            removed += 1;
        }
    }
    tx.commit().await?;
    Ok(
        json!({"added":added,"updated":updated,"removed":removed,"unchanged":unchanged,"skipped":skipped,"warnings":snapshot.warnings}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_buttons_use_live_panel_bindings_without_inventory_and_isolate_bad_inbounds() {
        let valid = json!({"id":10,"remark":"台湾","port":20337,"protocol":"vless","streamSettings":{
            "network":"tcp","security":"reality","realitySettings":{"settings":{"publicKey":"public-key"},
                "serverNames":["tls.example"],"shortIds":["ab12"]}}});
        let mut broken = valid.clone();
        broken["id"] = json!(11);
        broken["streamSettings"]["realitySettings"]["settings"] = json!({});
        let client = json!({"id":25,"uuid":"0840bd36-77f8-41a5-a472-07b67760d8a0","email":"小明","inboundIds":[10,11],"flow":"xtls-rprx-vision"});
        let (links, warnings) = client_links(&[valid, broken], &[client], "public.example");
        assert_eq!(links.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert_eq!(links[0]["clientId"], 25);
        assert_eq!(links[0]["inboundId"], 10);
        assert_eq!(links[0]["name"], "台湾 · 小明");
        let parsed = Url::parse(links[0]["link"].as_str().unwrap()).unwrap();
        assert_eq!(parsed.port(), Some(20337));
        assert_eq!(parsed.host_str(), Some("public.example"));
    }

    fn ib(id: i64) -> Value {
        json!({"id":id,"protocol":"vless","remark":format!("G{id}"),"port":20400+id,
            "streamSettings":{"network":"raw","security":"reality","realitySettings":{
                "settings":{"publicKey":"old-public","fingerprint":"firefox","spiderX":"/x"},
                "serverNames":["s.example"],"shortIds":["12"]}},"settings":"{\"clients\":[],\"decryption\":\"none\"}"})
    }
    fn client(id: i64, email: &str, ids: Vec<i64>) -> Value {
        json!({"id":id,"uuid":format!("00000000-1111-4000-8000-{id:012}"),"email":email,
            "flow":"","expiryTime":10,"enable":true,"inboundIds":ids})
    }
    #[tokio::test]
    async fn reconciles_binding_identity_names_links_and_deletions_and_preserves_failed_jobs(
    ) -> Result<()> {
        let tmp = tempfile::tempdir()?;
        // Start with the exact previous constraints, including globally unique UUIDs.
        let opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(tmp.path().join("nexus.sqlite"))
            .create_if_missing(true);
        let old = SqlitePool::connect_with(opts).await?;
        sqlx::raw_sql("CREATE TABLE panels(id TEXT PRIMARY KEY,name TEXT NOT NULL,url TEXT UNIQUE NOT NULL,region TEXT NOT NULL,token BLOB NOT NULL);
            INSERT INTO panels VALUES('p','P','http://p','',''),('q','Q','http://q','','');
            CREATE TABLE lines(id TEXT PRIMARY KEY,panel_id TEXT NOT NULL REFERENCES panels(id) ON DELETE CASCADE,
              name TEXT NOT NULL,uuid TEXT NOT NULL UNIQUE,status TEXT NOT NULL,expiry INTEGER NOT NULL,inbound_id INTEGER,
              link TEXT,error TEXT,updated INTEGER NOT NULL,renewal_target INTEGER,UNIQUE(panel_id,name));
            INSERT INTO lines VALUES('old','p','alice','00000000-1111-4000-8000-000000000003','ready',10,1,'old-link',NULL,100,NULL);
            INSERT INTO lines VALUES('draft','p','local','draft-uuid','failed',10,NULL,NULL,'x',100,NULL);").execute(&old).await?;
        old.close().await;
        let db = storage::open(tmp.path()).await?;
        let mut inbounds = vec![ib(1), ib(2), json!({"id":5,"protocol":"trojan"})];
        let mut clients = vec![client(3, "alice", vec![1, 2]), client(4, "bob", vec![1])];
        let snap = |ins: &[Value], cs: &[Value]| snapshot(ins, cs, "edge.example");
        let first = save(&db, "p", snap(&inbounds, &clients)?).await?;
        assert_eq!(
            (first["added"].as_i64(), first["updated"].as_i64()),
            (Some(2), Some(1))
        );
        let unchanged = save(&db, "p", snap(&inbounds, &clients)?).await?;
        assert_eq!(unchanged["unchanged"], 3);
        let other = save(&db, "q", snap(&inbounds, &clients)?).await?;
        assert_eq!(other["added"], 3);
        clients[0]["email"] = json!("新名");
        clients[0]["expiryTime"] = json!(2000);
        clients[0]["totalGB"] = json!(53_687_091_200_i64);
        clients[0]["inboundIds"] = json!([1]);
        inbounds[0]["remark"] = json!("新入站");
        inbounds[0]["port"] = json!(30444);
        inbounds[0]["streamSettings"]["realitySettings"]["settings"]["publicKey"] =
            json!("rotated-key");
        let changed = save(&db, "p", snap(&inbounds, &clients)?).await?;
        assert_eq!(changed["removed"], 1);
        assert_eq!(changed["updated"], 2);
        let row = sqlx::query("SELECT * FROM lines WHERE id='old'")
            .fetch_one(&db)
            .await?;
        assert_eq!(row.get::<String, _>("name"), "新入站 · 新名");
        assert_eq!(row.get::<i64, _>("expiry"), 2000);
        assert_eq!(row.get::<i64, _>("quota_bytes"), 53_687_091_200_i64);
        let link = Url::parse(row.get("link"))?;
        assert_eq!(link.port(), Some(30444));
        let params: HashMap<_, _> = link.query_pairs().collect();
        assert_eq!(params["pbk"], "rotated-key");
        assert_eq!(params["fp"], "firefox");
        assert!(!params.contains_key("flow"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM lines WHERE id='draft' AND status='failed'"
            )
            .fetch_one(&db)
            .await?,
            1
        );
        // A faulty remote snapshot with a duplicated record id must roll back all local edits.
        let mut corrupt = clients.clone();
        corrupt[1]["id"] = corrupt[0]["id"].clone();
        assert!(save(&db, "p", snap(&inbounds, &corrupt)?).await.is_err());
        assert_eq!(
            save(&db, "p", snap(&inbounds, &clients)?).await?["unchanged"],
            2
        );
        assert_eq!(save(&db, "q", snap(&[], &[])?).await?["removed"], 3);
        let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM lines WHERE panel_id='p'")
            .fetch_one(&db)
            .await?;
        assert_eq!(remaining, 3);
        db.close().await;
        Ok(())
    }
}
