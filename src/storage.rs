use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{sqlite::SqliteConnectOptions, Row, SqlitePool};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn data_directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NEXUS_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    Ok(base
        .context("请通过 NEXUS_DATA_DIR 指定数据目录")?
        .join("NexusPanel"))
}

pub async fn open(dir: &Path) -> Result<SqlitePool> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    let path = dir.join("nexus.sqlite");
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
    }
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::from_secs(15))
        .foreign_keys(true);
    let db = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await?;
    sqlx::raw_sql("
      CREATE TABLE IF NOT EXISTS panels(id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT UNIQUE NOT NULL, region TEXT NOT NULL, token BLOB NOT NULL);
      CREATE TABLE IF NOT EXISTS history(id TEXT PRIMARY KEY, ts INTEGER NOT NULL, action TEXT NOT NULL, results TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS client_deletions(
        panel_id TEXT NOT NULL REFERENCES panels(id) ON DELETE CASCADE, client_id INTEGER NOT NULL,
        email TEXT NOT NULL, uuid TEXT NOT NULL, plan TEXT NOT NULL, error TEXT NOT NULL,
        PRIMARY KEY(panel_id,client_id));
      CREATE TABLE IF NOT EXISTS client_extensions(
        panel_id TEXT NOT NULL REFERENCES panels(id) ON DELETE CASCADE, client_id INTEGER NOT NULL,
        email TEXT NOT NULL, uuid TEXT NOT NULL, base INTEGER NOT NULL, unit TEXT NOT NULL,
        updated_at INTEGER NOT NULL, amount INTEGER NOT NULL, target_expiry INTEGER NOT NULL,
        applied INTEGER NOT NULL DEFAULT 0,
        UNIQUE(panel_id,client_id,email,uuid,base,updated_at,unit,amount));
      CREATE TABLE IF NOT EXISTS counters(panel TEXT PRIMARY KEY REFERENCES panels(id) ON DELETE CASCADE, up INTEGER NOT NULL, down INTEGER NOT NULL, ts INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS traffic(panel TEXT NOT NULL REFERENCES panels(id) ON DELETE CASCADE, ts INTEGER NOT NULL, up INTEGER NOT NULL, down INTEGER NOT NULL);
      CREATE INDEX IF NOT EXISTS traffic_ts ON traffic(ts);
      CREATE TABLE IF NOT EXISTS lines(
        id TEXT PRIMARY KEY, panel_id TEXT NOT NULL REFERENCES panels(id) ON DELETE CASCADE,
        name TEXT NOT NULL, uuid TEXT NOT NULL UNIQUE, status TEXT NOT NULL,
        expiry INTEGER NOT NULL, inbound_id INTEGER, link TEXT, error TEXT, updated INTEGER NOT NULL,
        renewal_target INTEGER, UNIQUE(panel_id,name));
    ").execute(&db).await?;
    let mut tx = db.begin().await?;
    let sync_schema: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pragma_table_info('lines') WHERE name='client_email'",
    )
    .fetch_one(&mut *tx)
    .await?;
    if sync_schema == 0 {
        sqlx::raw_sql("
            ALTER TABLE lines RENAME TO lines_legacy;
            CREATE TABLE lines(
                id TEXT PRIMARY KEY, panel_id TEXT NOT NULL REFERENCES panels(id) ON DELETE CASCADE,
                name TEXT NOT NULL, uuid TEXT NOT NULL, status TEXT NOT NULL, expiry INTEGER NOT NULL,
                inbound_id INTEGER, link TEXT, error TEXT, updated INTEGER NOT NULL, renewal_target INTEGER,
                client_email TEXT NOT NULL DEFAULT '', inbound_name TEXT NOT NULL DEFAULT '',
                client_record INTEGER, shared_client INTEGER NOT NULL DEFAULT 0, enabled INTEGER NOT NULL DEFAULT 1,
                inbound_expiry INTEGER NOT NULL DEFAULT 0);
            INSERT INTO lines(id,panel_id,name,uuid,status,expiry,inbound_id,link,error,updated,renewal_target,client_email,inbound_name)
              SELECT id,panel_id,name,uuid,status,expiry,inbound_id,link,error,updated,renewal_target,name,name FROM lines_legacy;
            DROP TABLE lines_legacy;
            CREATE UNIQUE INDEX lines_pending_name ON lines(panel_id,name) WHERE status IN ('pending','failed');
            CREATE UNIQUE INDEX lines_client_binding ON lines(panel_id,inbound_id,client_record) WHERE client_record IS NOT NULL;
            CREATE INDEX lines_panel ON lines(panel_id);").execute(&mut *tx).await?;
    }
    let hosts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pragma_table_info('panels') WHERE name='public_host'",
    )
    .fetch_one(&mut *tx)
    .await?;
    if hosts == 0 {
        sqlx::query("ALTER TABLE panels ADD COLUMN public_host TEXT NOT NULL DEFAULT ''")
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(db)
}

#[derive(Clone, Serialize)]
pub struct Panel {
    pub id: String,
    pub name: String,
    pub url: String,
    pub region: String,
    #[serde(rename = "publicHost")]
    pub public_host: String,
    #[serde(skip)]
    secret: Vec<u8>,
}

impl Panel {
    pub fn api(&self, client: &reqwest::Client) -> Result<crate::upstream::PanelApi> {
        let token =
            String::from_utf8(protect(&self.secret, false)?).context("不能读取本机保存的令牌")?;
        crate::upstream::PanelApi::new(client.clone(), &self.url, &token)
    }
}

pub async fn panels(db: &SqlitePool) -> Result<Vec<Panel>> {
    let rows = sqlx::query("SELECT * FROM panels ORDER BY rowid")
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| Panel {
            id: r.get("id"),
            name: r.get("name"),
            url: r.get("url"),
            region: r.get("region"),
            public_host: r.get("public_host"),
            secret: r.get("token"),
        })
        .collect())
}

pub async fn insert_panel(
    db: &SqlitePool,
    name: &str,
    url: &str,
    region: &str,
    token: &str,
    public_host: &str,
) -> Result<Panel> {
    let p = Panel {
        id: Uuid::new_v4().to_string(),
        name: name.into(),
        url: url.into(),
        region: region.into(),
        public_host: public_host.into(),
        secret: protect(token.as_bytes(), true)?,
    };
    sqlx::query("INSERT INTO panels(id,name,url,region,token,public_host) VALUES(?,?,?,?,?,?)")
        .bind(&p.id)
        .bind(&p.name)
        .bind(&p.url)
        .bind(&p.region)
        .bind(&p.secret)
        .bind(&p.public_host)
        .execute(db)
        .await
        .map_err(|e| {
            if e.as_database_error()
                .is_some_and(|d| d.is_unique_violation())
            {
                anyhow!("此面板地址已接入")
            } else {
                anyhow!(e)
            }
        })?;
    Ok(p)
}

#[cfg(windows)]
fn protect(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into()?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut out = CRYPT_INTEGER_BLOB::default();
    // DPAPI allocates the output with LocalAlloc and scopes it to this Windows user.
    unsafe {
        let ok = if encrypt {
            CryptProtectData(
                &input,
                null(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        } else {
            CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let value = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData.cast());
        Ok(value)
    }
}

#[cfg(not(windows))]
fn protect(bytes: &[u8], _encrypt: bool) -> Result<Vec<u8>> {
    Ok(bytes.to_vec())
}

#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub panel_id: String,
    pub name: String,
    pub ok: bool,
    pub message: String,
}

pub async fn log_start(db: &SqlitePool, action: &str, entries: &[Outcome]) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO history VALUES(?,?,?,?)")
        .bind(&id)
        .bind(now())
        .bind(action)
        .bind(serde_json::to_string(entries)?)
        .execute(db)
        .await?;
    Ok(id)
}

pub async fn log_finish(db: &SqlitePool, id: &str, entries: &[Outcome]) -> Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE history SET results=? WHERE id=?")
        .bind(serde_json::to_string(entries)?)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM history WHERE rowid IN (SELECT rowid FROM history ORDER BY rowid DESC LIMIT -1 OFFSET 1000)").execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn history(db: &SqlitePool) -> Result<Vec<Value>> {
    let rows = sqlx::query("SELECT * FROM history ORDER BY rowid DESC LIMIT 200")
        .fetch_all(db)
        .await?;
    rows.into_iter().map(|r| Ok(json!({"id":r.get::<String,_>("id"),"ts":r.get::<i64,_>("ts"),"action":r.get::<String,_>("action"),"results":serde_json::from_str::<Value>(r.get("results"))?}))).collect()
}

pub async fn sample(db: &SqlitePool, id: &str, up: i64, down: i64, ts: i64) -> Result<()> {
    let mut tx = db.begin().await?;
    let last = sqlx::query("SELECT up,down,ts FROM counters WHERE panel=?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(row) = last {
        let passed = ts - row.get::<i64, _>("ts");
        if passed < 60_000 {
            return Ok(());
        }
        // An offline gap cannot be assigned to a particular hour on the chart.
        if passed <= 900_000 {
            let delta = |total: i64, prev: i64| (total - prev).max(0);
            sqlx::query("INSERT INTO traffic VALUES(?,?,?,?)")
                .bind(id)
                .bind(ts)
                .bind(delta(up, row.get("up")))
                .bind(delta(down, row.get("down")))
                .execute(&mut *tx)
                .await?;
        }
    }
    sqlx::query("INSERT OR REPLACE INTO counters VALUES(?,?,?,?)")
        .bind(id)
        .bind(up)
        .bind(down)
        .bind(ts)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM traffic WHERE ts < ?")
        .bind(ts - 32 * 86_400_000)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn traffic(db: &SqlitePool) -> Result<Vec<Value>> {
    let rows = sqlx::query("SELECT ts/3600000*3600000 AS hour,SUM(up) AS up,SUM(down) AS down FROM traffic GROUP BY hour ORDER BY hour").fetch_all(db).await?;
    Ok(rows.into_iter().map(|r| json!({"ts":r.get::<i64,_>("hour"),"up":r.get::<i64,_>("up"),"down":r.get::<i64,_>("down")})).collect())
}
