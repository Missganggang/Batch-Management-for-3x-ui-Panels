//! The wire formats in this adapter follow MHSanaei/3x-ui v3.4.2.
use anyhow::{anyhow, bail, ensure, Context, Result};
use reqwest::{header, Client, Method, RequestBuilder, Response, Url};
use serde_json::Value;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

pub struct BackupFile {
    pub file: std::fs::File,
    pub extension: &'static str,
    pub size: u64,
}

mod actions;
mod delete;
mod line;
mod reality;
mod repair_reality;
pub(crate) use line::share_link;

pub(crate) fn default_reality_target() -> String {
    std::env::var("NEXUS_REALITY_TARGET").unwrap_or_else(|_| "www.amd.com:443".into())
}
#[cfg(test)]
mod tests;

pub struct PanelApi {
    client: Client,
    base: Url,
    authorization: header::HeaderValue,
}

impl PanelApi {
    pub fn new(client: Client, base_url: &str, token: &str) -> Result<Self> {
        let mut base = Url::parse(base_url.trim()).context("面板地址无效")?;
        ensure!(
            matches!(base.scheme(), "http" | "https")
                && base.host().is_some()
                && base.username().is_empty()
                && base.password().is_none()
                && base.query().is_none()
                && base.fragment().is_none(),
            "请输入包含面板 Web 基础路径的 HTTP 或 HTTPS 地址"
        );
        let path = base.path().trim_end_matches('/');
        let path = path
            .strip_suffix("/panel/api")
            .or_else(|| path.strip_suffix("/panel"))
            .unwrap_or(path);
        let path = format!("{path}/panel/api/");
        base.set_path(&path);
        ensure!(!token.trim().is_empty(), "请输入 API 令牌");
        let mut authorization: header::HeaderValue = format!("Bearer {}", token.trim())
            .parse()
            .map_err(|_| anyhow!("API 令牌包含无效字符"))?;
        authorization.set_sensitive(true);
        Ok(Self {
            client,
            base,
            authorization,
        })
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let url = self.base.join(path).expect("static API path");
        self.request_url(method, url)
    }

    fn request_url(&self, method: Method, url: Url) -> RequestBuilder {
        self.client
            .request(method, url)
            .header(header::AUTHORIZATION, self.authorization.clone())
            .header("X-Requested-With", "XMLHttpRequest")
            .header(header::ACCEPT, "application/json, application/octet-stream")
    }

    fn item_url(&self, resource: &str, id: &str) -> Result<Url> {
        ensure!(
            !matches!(id, "" | "." | "..") && !id.contains(['/', '\\']),
            "对象标识无法用于面板地址"
        );
        let mut url = self.base.join(resource)?;
        url.path_segments_mut()
            .map_err(|_| anyhow!("面板地址无效"))?
            .pop_if_empty()
            .push(id);
        Ok(url)
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response> {
        let response = request.send().await.map_err(|err| {
            if err.is_timeout() {
                anyhow!("面板请求超时，请刷新并核对面板状态")
            } else if err.is_connect() {
                anyhow!("无法连接面板，请检查网络、地址及 TLS 证书")
            } else {
                anyhow!("面板请求中断，请刷新并核对面板状态")
            }
        })?;
        match response.status().as_u16() {
            200..=299 => Ok(response),
            401 | 403 => bail!("面板拒绝访问，请检查 API 令牌"),
            404 => bail!("未找到面板 API，请检查面板基础路径与版本"),
            status => bail!("面板返回 HTTP {status}"),
        }
    }

    async fn obj(&self, request: RequestBuilder) -> Result<Value> {
        let mut envelope: Value = self
            .send(request)
            .await?
            .json()
            .await
            .map_err(|_| anyhow!("面板没有返回有效的 3x-ui JSON 响应，请检查面板基础路径"))?;
        match envelope["success"].as_bool() {
            Some(true) => Ok(envelope["obj"].take()),
            Some(false) => {
                let mut reason = envelope["msg"]
                    .as_str()
                    .unwrap_or("面板操作失败")
                    .to_owned();
                if let Ok(auth) = self.authorization.to_str() {
                    if let Some(token) = auth.strip_prefix("Bearer ") {
                        reason = reason.replace(token, "[已隐藏]");
                    }
                }
                bail!("{}", reason.chars().take(400).collect::<String>());
            }
            None => bail!("响应缺少 3x-ui 操作状态，请检查面板基础路径"),
        }
    }

    async fn list(&self, path: &str) -> Result<Vec<Value>> {
        match self.obj(self.request(Method::GET, path)).await? {
            Value::Array(list) => Ok(list),
            Value::Null => Ok(Vec::new()), // An empty Go slice can serialize to null.
            _ => bail!("面板列表的数据格式无效"),
        }
    }

    pub async fn inbounds(&self) -> Result<Vec<Value>> {
        self.list("inbounds/list").await
    }

    pub async fn clients(&self) -> Result<Vec<Value>> {
        self.list("clients/list").await
    }

    async fn xray_settings(&self) -> Result<Value> {
        let obj = self.obj(self.request(Method::POST, "xray/")).await?;
        let result = match obj {
            Value::String(s) => serde_json::from_str(&s).context("面板 Xray 设置无法解析")?,
            value => value,
        };
        ensure!(
            result["xraySetting"].is_object(),
            "面板缺少可编辑的 Xray 配置模板"
        );
        Ok(result)
    }

    pub async fn config(&self) -> Result<Value> {
        Ok(self.xray_settings().await?["xraySetting"].take())
    }

    pub async fn backup(&self) -> Result<BackupFile> {
        let mut response = self
            .send(
                self.request(Method::GET, "server/getDb")
                    .timeout(std::time::Duration::from_secs(900)),
            )
            .await?;
        let mut file =
            tokio::fs::File::from_std(tempfile::tempfile().context("无法创建数据库临时文件")?);
        let mut magic = Vec::with_capacity(16);
        let mut size = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("备份下载中断"))?
        {
            magic.extend_from_slice(&chunk[..(16 - magic.len()).min(chunk.len())]);
            file.write_all(&chunk)
                .await
                .context("数据库临时文件写入失败")?;
            size += chunk.len() as u64;
        }
        let extension = if magic.starts_with(b"SQLite format 3\0") {
            "db"
        } else if magic.starts_with(b"PGDMP") {
            "dump"
        } else {
            bail!("面板未返回有效的 SQLite 或 PostgreSQL 数据库备份");
        };
        file.flush().await?;
        file.rewind().await?;
        Ok(BackupFile {
            file: file.into_std().await,
            extension,
            size,
        })
    }
}
