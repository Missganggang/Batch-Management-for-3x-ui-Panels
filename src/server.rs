use anyhow::{bail, Context, Result};
use axum::{
    extract::{Request, State},
    http::{header, uri::Authority, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    env,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};
use subtle::ConstantTimeEq;

pub fn bind_address() -> Result<SocketAddr> {
    let ip = env::var("NEXUS_BIND")
        .unwrap_or_else(|_| "127.0.0.1".into())
        .parse::<IpAddr>()
        .context("NEXUS_BIND 应为监听 IP 地址")?;
    let port = env::var("NEXUS_PORT")
        .unwrap_or_else(|_| "8787".into())
        .parse::<u16>()
        .context("NEXUS_PORT 应为有效端口")?;
    Ok(SocketAddr::new(ip, port))
}

#[derive(Clone)]
pub struct Access {
    address: SocketAddr,
    credential: Option<Arc<[u8]>>,
    panel_delete_digest: Option<[u8; 32]>,
}

impl Access {
    pub fn from_env(address: SocketAddr) -> Result<Self> {
        let read = |key| match env::var(key) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(_) => bail!("{key} 须为有效的 Unicode 文本"),
        };
        Self::new(
            address,
            read("NEXUS_ADMIN_PASSWORD")?,
            read("NEXUS_PANEL_DELETE_PASSWORD")?,
        )
    }

    fn new(
        address: SocketAddr,
        password: Option<String>,
        deletion: Option<String>,
    ) -> Result<Self> {
        let password = password.filter(|s| !s.trim().is_empty());
        if !address.ip().is_loopback() && password.is_none() {
            bail!("对外监听需要设置 NEXUS_ADMIN_PASSWORD");
        }
        Ok(Self {
            address,
            panel_delete_digest: deletion
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .or(password.as_deref())
                .map(|s| Sha256::digest(s.as_bytes()).into()),
            credential: password.map(|p| Arc::from(format!("admin:{p}").into_bytes())),
        })
    }

    pub fn verify_panel_delete(&self, candidate: &str) -> Result<(), &'static str> {
        let expected = self
            .panel_delete_digest
            .as_ref()
            .ok_or("尚未配置删除密码，请设置 NEXUS_PANEL_DELETE_PASSWORD 或管理员密码")?;
        let actual: [u8; 32] = Sha256::digest(candidate.as_bytes()).into();
        if bool::from(expected.ct_eq(&actual)) {
            Ok(())
        } else {
            Err("删除密码错误，请重新输入")
        }
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = &self.credential else {
            return true;
        };
        let Some((scheme, value)) = headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.split_once(' '))
        else {
            return false;
        };
        scheme.eq_ignore_ascii_case("Basic")
            && STANDARD
                .decode(value)
                .is_ok_and(|userpass| bool::from(expected.as_ref().ct_eq(&userpass)))
    }

    fn allowed_host(&self, host: &Authority) -> bool {
        if self.credential.is_some() {
            return true;
        }
        let local = host.host().eq_ignore_ascii_case("localhost")
            || host
                .host()
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        local && host.port_u16().unwrap_or(80) == self.address.port()
    }
}

fn allowed_origin(headers: &HeaderMap, host: &Authority) -> bool {
    if headers
        .get("sec-fetch-site")
        .is_some_and(|h| h == "cross-site" || h == "same-site")
    {
        return false;
    }
    let Some(header) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = header.to_str() else {
        return false;
    };
    // The TLS reverse proxy preserves the external Host. Validate against that
    // authority, so public HTTPS and local HTTP can use the same application.
    let Some((scheme, raw)) = origin.split_once("://") else {
        return false;
    };
    let default_port = match scheme {
        "https" => 443,
        "http" => 80,
        _ => return false,
    };
    let Ok(origin_host) = raw.parse::<Authority>() else {
        return false;
    };
    !raw.contains('@')
        && origin_host.host().eq_ignore_ascii_case(host.host())
        && origin_host.port_u16().unwrap_or(default_port) == host.port_u16().unwrap_or(default_port)
}

fn deny(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error":message}))).into_response()
}

pub async fn guard(State(access): State<Access>, req: Request, next: Next) -> Response {
    let headers = req.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .filter(|h| !h.contains('@'))
        .and_then(|h| h.parse::<Authority>().ok());
    let mut res = match host {
        Some(host) if access.allowed_host(&host) => {
            if req.uri().path().starts_with("/api/") && !allowed_origin(headers, &host) {
                deny(StatusCode::FORBIDDEN, "仅允许管理台同源访问")
            } else if !access.authorized(headers) {
                let mut r = deny(StatusCode::UNAUTHORIZED, "请使用管理员账号登录");
                r.headers_mut().insert(
                    header::WWW_AUTHENTICATE,
                    HeaderValue::from_static("Basic realm=\"Nexus\", charset=\"UTF-8\""),
                );
                r
            } else {
                next.run(req).await
            }
        }
        _ => deny(StatusCode::FORBIDDEN, "无效的管理台访问地址"),
    };
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("content-security-policy",HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; connect-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_mode_limits_hosts_and_remote_mode_requires_a_password() {
        let access = Access::new("127.0.0.1:8787".parse().unwrap(), None, None).unwrap();
        assert!(access.allowed_host(&"localhost:8787".parse().unwrap()));
        assert!(!access.allowed_host(&"foreign.example:8787".parse().unwrap()));
        assert!(!access.allowed_host(&"localhost:9000".parse().unwrap()));
        assert!(Access::new("0.0.0.0:8787".parse().unwrap(), None, None).is_err());
    }

    #[test]
    fn remote_access_verifies_basic_credentials_and_browser_origin() {
        let remote = Access::new(
            "0.0.0.0:8787".parse().unwrap(),
            Some("test-pass".into()),
            None,
        )
        .unwrap();
        let host = "nexus.example".parse().unwrap();
        let mut h = HeaderMap::new();
        assert!(!remote.authorized(&h));
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic YWRtaW46dGVzdC1wYXNz"),
        );
        assert!(remote.authorized(&h));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://nexus.example:443"),
        );
        assert!(allowed_origin(&h, &host));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://nexus.example:444"),
        );
        assert!(!allowed_origin(&h, &host));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://other.example"),
        );
        assert!(!allowed_origin(&h, &host));
    }

    #[test]
    fn panel_deletion_uses_override_then_login_and_fails_closed_without_either() {
        let address = "127.0.0.1:8787".parse().unwrap();
        for (login, deletion, expected) in [
            (None, None, None),
            (Some("  "), Some(""), None),
            (Some("admin-test"), Some("  "), Some("admin-test")),
            (Some("admin-test"), None, Some("admin-test")),
            (Some("admin-test"), Some(" 删除-test "), Some(" 删除-test ")),
            (None, Some("独立-test"), Some("独立-test")),
        ] {
            let access = Access::new(
                address,
                login.map(str::to_owned),
                deletion.map(str::to_owned),
            )
            .unwrap();
            for input in [
                "",
                "wrong",
                "admin-test",
                "删除-test",
                " 删除-test ",
                "独立-test",
            ] {
                assert_eq!(
                    access.verify_panel_delete(input).is_ok(),
                    Some(input) == expected
                );
            }
        }
    }
}
