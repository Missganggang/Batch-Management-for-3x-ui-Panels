mod batch;
mod clients;
mod lines;
mod outbounds;
mod panel_transfer;
mod server;
mod storage;
mod sync;
mod upstream;

use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, StatusCode, Uri},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Extension, Json, Router,
};
use futures_util::{stream, StreamExt};
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::{collections::HashMap, sync::Arc, time::Duration};
use storage::Panel;
use tokio::sync::{Mutex, Semaphore};
use upstream::PanelApi;

static WEB: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web");
type ApiResult<T> = std::result::Result<T, ApiError>;
type Shared = Arc<App>;

struct App {
    db: SqlitePool,
    client: reqwest::Client,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    traffic_lock: Mutex<()>,
    upstream_slots: Semaphore,
    backup_slot: Semaphore,
}
struct ApiError(StatusCode, String);
impl ApiError {
    fn bad(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    fn upstream(err: anyhow::Error) -> Self {
        Self(StatusCode::BAD_GATEWAY, format!("{err:#}"))
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        eprintln!("管理台错误: {err:#}");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "本机管理台未完成请求，请查看服务日志".into(),
        )
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}

impl App {
    async fn lock(&self, id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self
            .locks
            .lock()
            .await
            .entry(id.to_owned())
            .or_default()
            .clone();
        lock.lock_owned().await
    }
    async fn panel(&self, id: &str) -> ApiResult<Panel> {
        storage::panels(&self.db)
            .await?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "面板不存在或已移除".into()))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let bind = server::bind_address()?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .context("监听失败，请检查 NEXUS_BIND 和 NEXUS_PORT")?;
    let addr = listener.local_addr()?;
    let access = server::Access::from_env(addr)?;
    let dir = storage::data_directory()?;
    let state = Arc::new(App {
        db: storage::open(&dir).await?,
        client: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(6))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        locks: Mutex::new(HashMap::new()),
        traffic_lock: Mutex::new(()),
        upstream_slots: Semaphore::new(8),
        backup_slot: Semaphore::new(1),
    });
    let router = Router::new()
        .route("/api/panels", get(list_panels).post(add_panel))
        .route(
            "/api/panels/:id",
            delete(remove_panel)
                .put(edit_panel)
                .layer(Extension(access.clone())),
        )
        .route("/api/panels/:id/sync", post(sync_panel))
        .route("/api/panels/:id/note", post(update_panel_note))
        .route(
            "/api/panels/:id/repair-user-routes",
            post(lines::repair_user_routes),
        )
        .route("/api/panels/:id/client-links", get(clients::links))
        .route("/api/panels/:id/outbounds/test", post(outbounds::test))
        .route(
            "/api/panels/:id/outbounds/test-batch",
            post(outbounds::test_batch),
        )
        .route("/api/clients/delete", post(clients::delete))
        .route("/api/clients/export-xlsx", post(clients::export))
        .route("/api/clients/extend", post(clients::extend))
        .route("/api/clients/notes", post(clients::notes))
        .route("/api/clients/status", post(clients::status))
        .route("/api/clients/quota", post(clients::quota))
        .route("/api/clients/reset-traffic", post(clients::reset_traffic))
        .route("/api/clients/deletions", get(clients::pending))
        .route("/api/panels/:id/public-host", post(lines::change_host))
        .route("/api/lines", get(lines::list).post(lines::create))
        .route("/api/lines/batch", post(lines::create_batch))
        .route("/api/lines/qr", post(lines::qr))
        .route("/api/lines/:id/expiry", post(lines::renew))
        .route("/api/lines/:id/repair-reality", post(lines::repair_reality))
        .route("/api/lines/:id/test-outbound", post(lines::test_outbound))
        .route("/api/panels/:id/:resource", get(resource))
        .route("/api/overview", get(overview))
        .route("/api/history", get(history))
        .route("/api/batch", post(batch::execute))
        .route("/api/backup", post(batch::backup))
        .route("/api/panel-transfer/export", post(panel_transfer::export))
        .route(
            "/api/panel-transfer/restore",
            post(panel_transfer::restore).layer(DefaultBodyLimit::max(4 * 1024 * 1024 + 16 * 1024)),
        )
        .fallback(get(asset))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(access, server::guard))
        .with_state(state.clone());
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(err) = collect_overview(&state).await {
                eprintln!("采集流量失败: {err:#}");
            }
        }
    });
    println!(
        "Nexus 面板管理台   http://{addr}\n本地数据目录   {}",
        dir.display()
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let Some(file) = WEB.get_file(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    };
    ([(header::CONTENT_TYPE, mime)], Body::from(file.contents())).into_response()
}

async fn list_panels(State(app): State<Shared>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"panels":storage::panels(&app.db).await?})))
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PanelForm {
    #[serde(default)]
    name: String,
    url: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    public_host: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    owner: String,
}

impl PanelForm {
    fn normalize(&mut self, saved_base: Option<&str>) -> ApiResult<reqwest::Url> {
        let mut url = reqwest::Url::parse(self.url.trim())
            .map_err(|_| ApiError::bad("请输入有效的面板地址"))?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || self.url.len() > 2048
        {
            return Err(ApiError::bad(
                "请输入包含面板 Web 基础路径的 HTTP 或 HTTPS 地址",
            ));
        }
        if Some(url.as_str().trim_end_matches('/')) != saved_base {
            let path = url.path().trim_end_matches('/');
            let path = path
                .strip_suffix("/panel/api")
                .or_else(|| path.strip_suffix("/panel"))
                .unwrap_or(path)
                .to_owned();
            url.set_path(&path);
        }
        self.url = url.as_str().trim_end_matches('/').to_owned();
        self.name = self.name.trim().to_owned();
        if self.name.is_empty() {
            self.name = url.host_str().expect("validated host").to_owned();
        }
        self.region = self.region.trim().to_owned();
        self.owner = self.owner.trim().to_owned();
        self.token = self.token.trim().to_owned();
        self.public_host = lines::public_host(&self.public_host)?;
        self.note = panel_note(&self.note)?.to_owned();
        if [self.name.as_str(), &self.region, &self.owner]
            .iter()
            .any(|s| s.chars().count() > 80)
        {
            return Err(ApiError::bad(
                "面板名称、地区和服务器归属最多可填写 80 个字符",
            ));
        }
        if self.token.len() > 8192 {
            return Err(ApiError::bad("API 令牌过长"));
        }
        Ok(url)
    }
}

#[derive(Deserialize)]
struct AddPanel {
    #[serde(flatten)]
    form: PanelForm,
    #[serde(default)]
    sync: bool,
}

async fn verify_panel(app: &App, panel: &Panel) -> ApiResult<()> {
    let api = panel
        .api(&app.client)
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
    api.inbounds().await.map_err(ApiError::upstream)?;
    Ok(())
}

async fn add_panel(
    State(app): State<Shared>,
    Json(mut p): Json<AddPanel>,
) -> ApiResult<impl IntoResponse> {
    p.form.normalize(None)?;
    let panel = Panel::from_form(p.form, None).map_err(|e| ApiError::bad(e.to_string()))?;
    verify_panel(&app, &panel).await?;
    storage::insert_panel(&app.db, &panel)
        .await
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let mut result = json!({"panel":panel});
    if p.sync {
        match sync::run(&app, &panel.id).await {
            Ok(report) => result["sync"] = report,
            Err(err) => result["syncError"] = json!(err.1),
        }
    }
    Ok((StatusCode::CREATED, Json(result)))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditPanel {
    expected_revision: i64,
    #[serde(flatten)]
    form: PanelForm,
}

async fn edit_panel(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(mut input): Json<EditPanel>,
) -> ApiResult<Json<Value>> {
    let _guard = app.lock(&id).await;
    let old = app.panel(&id).await?;
    let url = input.form.normalize(Some(&old.url))?;
    let conflict = || {
        ApiError(
            StatusCode::CONFLICT,
            "面板资料已变更，请刷新后再编辑".into(),
        )
    };
    if old.revision != input.expected_revision {
        return Err(conflict());
    }
    let has_token = !input.form.token.is_empty();
    if !has_token
        && reqwest::Url::parse(&old.url)
            .context("已保存的面板地址无效")?
            .origin()
            != url.origin()
    {
        return Err(ApiError::bad(
            "更改面板域名、协议或端口时，请填写目标面板的 API 令牌",
        ));
    }
    let candidate = Panel::from_form(input.form, Some(&old))?;
    if has_token || candidate.url != old.url {
        verify_panel(&app, &candidate).await?;
    }
    let mut tx = app.db.begin().await.context("无法更新面板资料")?;
    if !storage::update_panel(&mut tx, &candidate, old.revision)
        .await
        .map_err(|e| ApiError::bad(e.to_string()))?
    {
        return Err(conflict());
    }
    if sync::sharing_host(&old)? != sync::sharing_host(&candidate)? {
        lines::update_share_links(&mut tx, &candidate).await?;
    }
    tx.commit().await.context("无法保存面板资料")?;
    Ok(Json(json!({"panel":candidate})))
}

async fn sync_panel(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(sync::run(&app, &id).await?))
}

fn panel_note(value: &str) -> ApiResult<&str> {
    let note = value.trim();
    if note.chars().count() > 2048 {
        return Err(ApiError::bad("面板备注最多可填写 2048 个字符"));
    }
    Ok(note)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PanelNote {
    note: String,
    expected_note: String,
}

async fn update_panel_note(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(input): Json<PanelNote>,
) -> ApiResult<Json<Value>> {
    let note = panel_note(&input.note)?;
    let update = sqlx::query(
        "UPDATE panels SET revision=revision+(note<>?), note=? WHERE id=? AND note IN (?,?)",
    )
    .bind(note)
    .bind(note)
    .bind(&id)
    .bind(&input.expected_note)
    .bind(note)
    .execute(&app.db)
    .await
    .context("保存面板备注失败")?;
    let panel = app.panel(&id).await?;
    if update.rows_affected() == 0 {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "面板备注已变更，请刷新后再编辑".into(),
        ));
    }
    Ok(Json(json!({"note":panel.note,"panel":panel})))
}

#[derive(Deserialize)]
struct DeletePanel {
    #[serde(default)]
    password: String,
}
async fn remove_panel(
    State(app): State<Shared>,
    Extension(access): Extension<server::Access>,
    Path(id): Path<String>,
    Json(input): Json<DeletePanel>,
) -> ApiResult<Json<Value>> {
    access
        .verify_panel_delete(&input.password)
        .map_err(|msg| ApiError(StatusCode::FORBIDDEN, msg.into()))?;
    let _guard = app.lock(&id).await;
    let changed = sqlx::query("DELETE FROM panels WHERE id=?")
        .bind(id)
        .execute(&app.db)
        .await
        .context("移除连接失败")?;
    if changed.rows_affected() == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "该连接已移除".into()));
    }
    Ok(Json(json!({"ok":true})))
}
async fn resource(
    State(app): State<Shared>,
    Path((id, resource)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let api = app.panel(&id).await?.api(&app.client)?;
    let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
    let body = match resource.as_str() {
        "inbounds" => json!({"inbounds":api.inbounds().await.map_err(ApiError::upstream)?}),
        "clients" => json!({"clients":api.clients().await.map_err(ApiError::upstream)?}),
        "client-count" => json!({"count":api.clients().await.map_err(ApiError::upstream)?.len()}),
        "online-clients" => {
            json!({"emails":api.online_clients().await.map_err(ApiError::upstream)?})
        }
        "config" => json!({"config":api.config().await.map_err(ApiError::upstream)?}),
        _ => return Err(ApiError(StatusCode::NOT_FOUND, "资源不存在".into())),
    };
    Ok(Json(body))
}
async fn collect_overview(app: &App) -> Result<Value> {
    let _guard = app.traffic_lock.lock().await;
    let panels = storage::panels(&app.db).await?;
    let rows: Vec<Value> = stream::iter(panels)
        .map(|panel| async move {
            let mut row = json!(panel);
            let stats: Result<_> = async {
                let api = panel.api(&app.client)?;
                let _slot = app.upstream_slots.acquire().await?;
                let (inbounds, clients) = tokio::try_join!(api.inbounds(), api.clients())?;
                let total = |field: &str| {
                    inbounds
                        .iter()
                        .filter_map(|i| i[field].as_i64())
                        .filter(|v| *v >= 0)
                        .fold(0i64, i64::saturating_add)
                };
                Ok((inbounds.len(), clients.len(), total("up"), total("down")))
            }
            .await;
            match stats {
                Ok((inbounds, clients, up, down)) => {
                    row["online"] = json!(true);
                    row["inbounds"] = json!(inbounds);
                    row["clients"] = json!(clients);
                    row["up"] = json!(up);
                    row["down"] = json!(down);
                    if let Err(err) =
                        storage::sample(&app.db, &panel.id, up, down, storage::now()).await
                    {
                        eprintln!("流量采样未保存: {err}");
                    }
                }
                Err(err) => {
                    row["online"] = json!(false);
                    row["error"] = json!(format!("{err:#}"));
                }
            }
            row
        })
        .buffered(6)
        .collect()
        .await;
    Ok(json!({"panels":rows,"history":storage::traffic(&app.db).await?}))
}
async fn overview(State(app): State<Shared>) -> ApiResult<Json<Value>> {
    Ok(Json(collect_overview(&app).await?))
}
async fn history(State(app): State<Shared>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"history":storage::history(&app.db).await?})))
}
