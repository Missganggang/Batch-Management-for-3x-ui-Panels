use crate::{upstream::ProbeMode, ApiError, ApiResult, Shared};
use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Deserialize)]
pub struct Probe {
    tag: String,
    mode: ProbeMode,
}

#[derive(Deserialize)]
pub struct BatchProbe {
    tags: Vec<String>,
    mode: ProbeMode,
}

pub async fn test(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(input): Json<Probe>,
) -> ApiResult<Json<Value>> {
    if input.tag.trim().is_empty() {
        return Err(ApiError::bad("请选择要测试的 SOCKS5 出站"));
    }
    let api = app.panel(&id).await?.api(&app.client)?;
    let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
    let ms = api
        .test_socks_outbound(&input.tag, input.mode)
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(json!({"latencyMs":ms})))
}

pub async fn test_batch(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(input): Json<BatchProbe>,
) -> ApiResult<Json<Value>> {
    let mut seen = HashSet::new();
    if !(1..=50).contains(&input.tags.len())
        || input
            .tags
            .iter()
            .any(|tag| tag.trim().is_empty() || !seen.insert(tag))
    {
        return Err(ApiError::bad("每批请选择 1 至 50 个不同的 SOCKS5 出站"));
    }
    let api = app.panel(&id).await?.api(&app.client)?;
    let _slot = app.upstream_slots.acquire().await.expect("open semaphore");
    let tags: Vec<_> = input.tags.iter().map(String::as_str).collect();
    let outcomes = api
        .test_socks_batch(&tags, input.mode)
        .await
        .map_err(ApiError::upstream)?;
    let results: Vec<_> = tags
        .into_iter()
        .zip(outcomes)
        .map(|(tag, outcome)| match outcome {
            Ok(ms) => json!({"tag":tag, "latencyMs":ms}),
            Err(error) => json!({"tag":tag, "error":error}),
        })
        .collect();
    Ok(Json(json!({"results":results})))
}
