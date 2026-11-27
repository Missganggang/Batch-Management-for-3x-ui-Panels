use crate::{lines, storage, ApiError, ApiResult, Shared};
use anyhow::Context;
use axum::{
    extract::{rejection::JsonRejection, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::{
    aead, pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use std::{collections::HashSet, num::NonZeroU32};
use tokio::sync::Semaphore;
use zeroize::{Zeroize, Zeroizing};

const MAX_FILE: usize = 4 * 1024 * 1024;
const MAX_RECORDS: usize = 512;
const ROUNDS: NonZeroU32 = NonZeroU32::new(600_000).unwrap();
// Version 1 fixes the cipher, KDF and iteration count. The format header is
// authenticated as AAD and unsupported versions never select weaker algorithms.
const FORMAT: &str = "nexus-panel-transfer";
const AAD: &[u8] = b"nexus-panel-transfer/v1/PBKDF2-HMAC-SHA256-600000/AES-256-GCM";
static CRYPTO_SLOTS: Semaphore = Semaphore::const_new(2);

#[derive(Deserialize)]
pub struct Export {
    password: String,
}

#[derive(Deserialize)]
pub struct Restore {
    password: String,
    backup: Envelope,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: String,
    version: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    id: String,
    name: String,
    owner: String,
    url: String,
    region: String,
    public_host: String,
    note: String,
    token: String,
}

impl Drop for Record {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

fn password(value: String) -> ApiResult<Zeroizing<String>> {
    let value = Zeroizing::new(value);
    if value.trim().is_empty() || value.len() > 1024 {
        Err(ApiError::bad("请输入备份密码，最多 1024 个 UTF-8 字节"))
    } else {
        Ok(value)
    }
}

fn derive(password: &str, salt: &[u8]) -> aead::LessSafeKey {
    let mut raw = Zeroizing::new([0u8; 32]);
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        ROUNDS,
        salt,
        password.as_bytes(),
        raw.as_mut(),
    );
    aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, raw.as_ref()).expect("32-byte AES key"),
    )
}

fn encrypt(records: &[Record], password: &str) -> ApiResult<Vec<u8>> {
    let mut data = Zeroizing::new(serde_json::to_vec(records).context("序列化接入配置失败")?);
    if data.len() > MAX_FILE * 3 / 4 {
        return Err(ApiError::bad("接入配置备份超过 4 MiB"));
    }
    let rng = SystemRandom::new();
    let mut salt = [0; 16];
    let mut nonce = [0; 12];
    rng.fill(&mut salt)
        .and_then(|()| rng.fill(&mut nonce))
        .map_err(|_| anyhow::anyhow!("生成备份随机数失败"))?;
    derive(password, &salt)
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(AAD),
            &mut *data,
        )
        .map_err(|_| anyhow::anyhow!("加密接入配置失败"))?;
    let bytes = serde_json::to_vec(&Envelope {
        format: FORMAT.into(),
        version: 1,
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce),
        ciphertext: STANDARD.encode(data.as_slice()),
    })
    .context("生成备份文件失败")?;
    if bytes.len() > MAX_FILE {
        return Err(ApiError::bad("接入配置备份超过 4 MiB"));
    }
    Ok(bytes)
}

fn decrypt(envelope: Envelope, password: &str) -> ApiResult<Vec<Record>> {
    if envelope.format != FORMAT || envelope.version != 1 {
        return Err(ApiError::bad("不支持此备份格式或版本"));
    }
    if serde_json::to_vec(&envelope)
        .context("读取备份格式失败")?
        .len()
        > MAX_FILE
    {
        return Err(ApiError::bad("接入配置备份超过 4 MiB"));
    }
    let corrupt = || ApiError::bad("备份密码错误或文件已损坏");
    let salt: [u8; 16] = STANDARD
        .decode(envelope.salt)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(corrupt)?;
    let nonce: [u8; 12] = STANDARD
        .decode(envelope.nonce)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(corrupt)?;
    let mut data = Zeroizing::new(
        STANDARD
            .decode(envelope.ciphertext)
            .map_err(|_| corrupt())?,
    );
    let clear = derive(password, &salt)
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(AAD),
            &mut data,
        )
        .map_err(|_| corrupt())?;
    let records: Vec<Record> =
        serde_json::from_slice(clear).map_err(|_| ApiError::bad("备份内的接入配置格式无效"))?;
    if records.len() > MAX_RECORDS {
        return Err(ApiError::bad("每个备份最多支持 512 个面板"));
    }
    Ok(records)
}

fn validate(record: &mut Record) -> ApiResult<()> {
    let invalid = || ApiError::bad("备份包含无效的面板记录");
    if record.id.is_empty()
        || record.id.len() > 100
        || !record
            .id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        || record.name.trim().is_empty()
        || record.name.chars().count() > 80
        || record.owner.chars().count() > 80
        || record.region.chars().count() > 80
        || record.note.chars().count() > 2048
        || record.url.len() > 2048
        || record.token.trim().is_empty()
        || record.token.len() > 8192
        || !record
            .token
            .bytes()
            .all(|c| (0x21..=0x7e).contains(&c) || c == b' ')
    {
        return Err(invalid());
    }
    let url = reqwest::Url::parse(&record.url).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err(invalid());
    }
    // A stored URL already has the management API suffix removed. A legitimate
    // custom base can itself end in "/panel"; never strip it a second time.
    record.url = url.as_str().trim_end_matches('/').to_owned();
    record.public_host = lines::public_host(&record.public_host).map_err(|_| invalid())?;
    Ok(())
}

async fn export_records(db: &SqlitePool) -> ApiResult<Vec<Record>> {
    let rows=sqlx::query("SELECT id,name,owner,url,region,public_host,note,token FROM panels ORDER BY rowid LIMIT 513").fetch_all(db).await.context("读取面板接入配置失败")?;
    if rows.len() > MAX_RECORDS {
        return Err(ApiError::bad("每个备份最多支持 512 个面板"));
    }
    rows.into_iter()
        .map(|r| {
            let token = storage::protect(r.get("token"), false)
                .map_err(|_| anyhow::anyhow!("无法读取本机保护的面板令牌"))?;
            let token = String::from_utf8(token)
                .map_err(|_| anyhow::anyhow!("无法读取本机保护的面板令牌"))?;
            let mut record = Record {
                id: r.get("id"),
                name: r.get("name"),
                owner: r.get("owner"),
                url: r.get("url"),
                region: r.get("region"),
                public_host: r.get("public_host"),
                note: r.get("note"),
                token,
            };
            validate(&mut record)?;
            Ok(record)
        })
        .collect()
}

async fn restore_records(db: &SqlitePool, records: Vec<Record>) -> ApiResult<Value> {
    let mut ids = HashSet::new();
    let mut urls = HashSet::new();
    let mut added = 0;
    let mut skipped = 0;
    let conflict = || {
        ApiError(
            StatusCode::CONFLICT,
            "备份中的面板标识与地址冲突，未导入任何记录".into(),
        )
    };
    let mut tx = db
        .begin_with("BEGIN IMMEDIATE")
        .await
        .context("开始恢复接入配置失败")?;
    for mut record in records {
        validate(&mut record)?;
        if !ids.insert(record.id.clone()) || !urls.insert(record.url.clone()) {
            return Err(conflict());
        }
        let hits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM panels WHERE id=? OR url=?")
            .bind(&record.id)
            .bind(&record.url)
            .fetch_one(&mut *tx)
            .await
            .context("核对面板记录失败")?;
        if hits > 1 {
            return Err(conflict());
        }
        if hits == 1 {
            skipped += 1;
            continue;
        }
        let protected = storage::protect(record.token.as_bytes(), true)
            .map_err(|_| anyhow::anyhow!("无法在目标服务器保存令牌"))?;
        sqlx::query("INSERT INTO panels(id,name,owner,url,region,public_host,note,token) VALUES(?,?,?,?,?,?,?,?)")
            .bind(&record.id).bind(&record.name).bind(&record.owner).bind(&record.url).bind(&record.region)
            .bind(&record.public_host).bind(&record.note).bind(protected)
            .execute(&mut *tx).await.context("写入面板接入配置失败")?;
        added += 1;
    }
    tx.commit().await.context("完成面板迁移失败")?;
    Ok(json!({"added":added,"skipped":skipped}))
}

fn crypto_slot() -> ApiResult<tokio::sync::SemaphorePermit<'static>> {
    CRYPTO_SLOTS.try_acquire().map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "正在处理其他配置备份，请稍后重试".into(),
        )
    })
}

fn bad_json(err: JsonRejection) -> ApiError {
    ApiError(err.status(), "备份请求格式无效或超过大小限制".into())
}

pub async fn export(
    State(app): State<Shared>,
    request: Result<Json<Export>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(request) = request.map_err(bad_json)?;
    let pass = password(request.password)?;
    let slot = crypto_slot()?;
    let records = export_records(&app.db).await?;
    let bytes = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        encrypt(&records, &pass)
    })
    .await
    .context("生成面板备份任务中断")??;
    let filename = format!(
        "attachment; filename=\"nexus-panels-{}.json\"",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CONTENT_DISPOSITION, filename.as_str()),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}

pub async fn restore(
    State(app): State<Shared>,
    request: Result<Json<Restore>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    let Json(request) = request.map_err(bad_json)?;
    let pass = password(request.password)?;
    let slot = crypto_slot()?;
    let records = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        decrypt(request.backup, &pass)
    })
    .await
    .context("读取备份任务中断")??;
    Ok(Json(restore_records(&app.db, records).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, host: &str, token: &str) -> Record {
        Record {
            id: id.into(),
            name: format!("面板 {id}"),
            owner: "管理者".into(),
            url: format!("https://{host}.invalid/custom/panel"),
            region: "香港".into(),
            public_host: "share.example".into(),
            note: "备注 🪴".into(),
            token: token.into(),
        }
    }
    fn ok<T>(result: ApiResult<T>) -> T {
        result.unwrap_or_else(|e| panic!("{}", e.1))
    }

    #[tokio::test]
    async fn migrates_between_databases_reencrypts_tokens_and_keeps_existing_credentials() {
        let from = tempfile::tempdir().unwrap();
        let to = tempfile::tempdir().unwrap();
        let source = storage::open(from.path()).await.unwrap();
        let target = storage::open(to.path()).await.unwrap();
        let source_records = vec![
            record("one", "first", "source-a"),
            record("two", "second", "source-b"),
            record("three", "third", "source-c"),
        ];
        ok(restore_records(&source, source_records).await);
        let originals = ok(export_records(&source).await);
        let pw = " 跨机测试 😀 ";
        let encoded = ok(encrypt(&originals, pw));
        let copy = ok(encrypt(&originals, pw));
        assert_ne!(encoded, copy);
        for secret in ["source-a", "first.invalid", "管理者", "备注"] {
            assert!(!std::str::from_utf8(&encoded).unwrap().contains(secret));
        }
        let restored = || ok(decrypt(serde_json::from_slice(&encoded).unwrap(), pw));
        let old_records = vec![
            record("one", "local", "local-a"),
            record("four", "second", "local-b"),
        ];
        ok(restore_records(&target, old_records).await);
        assert_eq!(
            ok(restore_records(&target, restored()).await),
            json!({"added":1,"skipped":2})
        );
        assert_eq!(
            ok(restore_records(&target, restored()).await),
            json!({"added":0,"skipped":3})
        );
        let records = ok(export_records(&target).await);
        assert_eq!(records.len(), 3);
        for (id, host, token) in [
            ("one", "local", "local-a"),
            ("four", "second", "local-b"),
            ("three", "third", "source-c"),
        ] {
            let actual = records.iter().find(|r| r.id == id).unwrap();
            assert_eq!(actual.url, format!("https://{host}.invalid/custom/panel"));
            assert_eq!(actual.token, token);
            assert_eq!(actual.owner, "管理者");
            assert_eq!(actual.region, "香港");
            assert_eq!(actual.public_host, "share.example");
            assert_eq!(actual.note, "备注 🪴");
        }
        #[cfg(windows)]
        {
            let raw = |db: SqlitePool| async move {
                sqlx::query_scalar::<_, Vec<u8>>("SELECT token FROM panels WHERE id='three'")
                    .fetch_one(&db)
                    .await
                    .unwrap()
            };
            assert_ne!(raw(source.clone()).await, raw(target.clone()).await);
        }
        let wrong = decrypt(serde_json::from_slice(&encoded).unwrap(), "wrong-password")
            .err()
            .unwrap();
        let mut broken: Envelope = serde_json::from_slice(&encoded).unwrap();
        let mut bytes = STANDARD.decode(broken.ciphertext).unwrap();
        bytes[0] ^= 1;
        broken.ciphertext = STANDARD.encode(bytes);
        let damaged = decrypt(broken, pw).err().unwrap();
        assert_eq!(wrong.0, StatusCode::BAD_REQUEST);
        assert_eq!(wrong.1, damaged.1);
    }

    #[tokio::test]
    async fn rollback_on_cross_conflict_duplicate_in_file_or_invalid_record() {
        let dir = tempfile::tempdir().unwrap();
        let db = storage::open(dir.path()).await.unwrap();
        ok(restore_records(
            &db,
            vec![
                record("a", "first", "a-token"),
                record("b", "second", "b-token"),
            ],
        )
        .await);
        for (bad, status) in [
            (record("a", "second", "bad"), StatusCode::CONFLICT),
            (record("new", "different", "bad"), StatusCode::CONFLICT),
            (record("z", "invalid", ""), StatusCode::BAD_REQUEST),
        ] {
            let result = restore_records(&db, vec![record("new", "new", "other-token"), bad]).await;
            assert_eq!(result.err().unwrap().0, status);
            let records = ok(export_records(&db).await);
            assert_eq!(records.len(), 2);
            assert_eq!(records[0].token, "a-token");
            assert_eq!(records[1].token, "b-token");
        }
        assert!(password("  ".into()).is_err());
        assert!(password("a".repeat(1025)).is_err());
    }
}
