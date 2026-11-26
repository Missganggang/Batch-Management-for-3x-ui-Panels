use crate::{ApiError, ApiResult};
use anyhow::Context;
use axum::{
    body::Body,
    http::{header, StatusCode},
    response::Response,
    Json,
};
use rust_xlsxwriter::{ExcelDateTime, Format, FormatAlign, Image, ObjectMovement, Workbook};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    collections::HashSet,
    fs::File,
    io::{Seek, Write},
};
use tokio::sync::Semaphore;
use tokio_util::io::ReaderStream;

const MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
static BUILD_SLOTS: Semaphore = Semaphore::const_new(2);

#[derive(Deserialize, Debug)]
pub struct ExportRows {
    columns: Vec<String>,
    rows: Vec<Map<String, Value>>,
}

fn column(id: &str) -> Option<(&'static str, u16)> {
    Some(match id {
        "name" => ("节点名称", 190),
        "link" => ("VLESS 链接", 460),
        "expiry" => ("到期时间", 195),
        "qr" => ("二维码", 216),
        "panel" => ("所属面板", 162),
        "inbound" => ("关联入站", 190),
        "note" => ("备注", 270),
        "host" => ("服务器地址", 215),
        "port" => ("端口", 82),
        "status" => ("客户端状态", 108),
        _ => return None,
    })
}

fn node_link(value: &str) -> bool {
    value.starts_with("vless://")
        && !value.chars().any(char::is_control)
        && reqwest::Url::parse(value).is_ok_and(|u| u.has_host() && !u.username().is_empty())
}

fn port(value: &Value) -> Option<u16> {
    match value {
        Value::Number(n) => n.as_u64().and_then(|v| u16::try_from(v).ok()),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
    .filter(|n| *n > 0)
}

fn validate(input: &ExportRows) -> ApiResult<()> {
    let mut seen = HashSet::new();
    if input.columns.is_empty()
        || input
            .columns
            .iter()
            .any(|key| column(key).is_none() || !seen.insert(key.as_str()))
    {
        return Err(ApiError::bad("请选择有效且不重复的导出字段"));
    }
    if input.rows.is_empty() || input.rows.len() > 10000 {
        return Err(ApiError::bad("每次可导出 1 至 10000 行"));
    }
    for (index, row) in input.rows.iter().enumerate() {
        for id in &input.columns {
            let value = row.get(id).unwrap_or(&Value::Null);
            if value.is_null() || value.as_str() == Some("") {
                continue;
            }
            let valid = if id == "port" {
                port(value).is_some()
            } else {
                value.as_str().is_some_and(|s| {
                    s.encode_utf16().count() <= 32767
                        && (!matches!(id.as_str(), "link" | "qr") || node_link(s))
                })
            };
            if !valid {
                return Err(ApiError::bad(format!(
                    "第 {} 行的{}格式无效或超过 Excel 单元格长度",
                    index + 1,
                    column(id).unwrap().0
                )));
            }
        }
        if seen.contains("qr")
            && seen.contains("link")
            && row.get("link").and_then(Value::as_str).unwrap_or_default()
                != row.get("qr").and_then(Value::as_str).unwrap_or_default()
        {
            return Err(ApiError::bad(format!(
                "第 {} 行二维码和 VLESS 链接不同",
                index + 1
            )));
        }
    }
    Ok(())
}

fn excel_date(value: &str) -> Option<ExcelDateTime> {
    // Only the frontend's local wall time is promoted to a date. Text expiry
    // modes and dates outside Excel's date range retain their exact wording.
    if value.len() != 19 || value.as_bytes().get(10) != Some(&b'T') {
        return None;
    }
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").ok()?;
    ExcelDateTime::parse_from_str(value).ok()
}

fn build(input: ExportRows) -> ApiResult<File> {
    validate(&input)?;
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("节点清单").context("设置工作表标题失败")?;
    sheet.set_freeze_panes(1, 0).context("冻结表头失败")?;
    sheet
        .set_row_height_pixels(0, 34)
        .context("设置表头高度失败")?;
    let header = Format::new()
        .set_bold()
        .set_font_color("#FFFFFF")
        .set_background_color("#23564C")
        .set_align(FormatAlign::VerticalCenter);
    let text = Format::new().set_align(FormatAlign::VerticalCenter);
    let link_format = text.clone().set_text_wrap();
    let date = text.clone().set_num_format("yyyy-mm-dd hh:mm:ss");
    for (i, id) in input.columns.iter().enumerate() {
        let (label, width) = column(id).unwrap();
        sheet
            .set_column_width_pixels(i as u16, width)
            .context("设置列宽失败")?;
        sheet
            .write_string_with_format(0, i as u16, label, &header)
            .context("写入表头失败")?;
    }
    let row_count = input.rows.len() as u32;
    for (index, row) in input.rows.into_iter().enumerate() {
        let n = index as u32 + 1;
        for (i, id) in input.columns.iter().enumerate() {
            let col = i as u16;
            let value = row.get(id).unwrap_or(&Value::Null);
            if value.is_null() || value.as_str() == Some("") {
                continue;
            }
            let s = value.as_str().unwrap_or_default();
            match id.as_str() {
                "qr" => {
                    let png = super::qr_png::node_qr(s).map_err(|_| {
                        ApiError::bad(format!(
                            "第 {n} 行的链接超出二维码容量，可仅导出 VLESS 链接列"
                        ))
                    })?;
                    let image = Image::new_from_buffer(&png)
                        .context("读取二维码 PNG 失败")?
                        .set_scale_to_size(200, 200, true)
                        .set_object_movement(ObjectMovement::MoveAndSizeWithCells)
                        .set_alt_text(format!("第 {n} 行节点二维码"));
                    sheet
                        .set_row_height_pixels(n, 216)
                        .context("设置二维码行高失败")?;
                    sheet
                        .insert_image_with_offset(n, col, &image, 8, 8)
                        .context("插入二维码失败")?;
                }
                "port" => {
                    sheet
                        .write_number_with_format(n, col, port(value).unwrap(), &text)
                        .context("写入端口失败")?;
                }
                "link" => {
                    sheet
                        .write_string_with_format(n, col, s, &link_format)
                        .context("写入 VLESS 链接失败")?;
                }
                _ => {
                    if let Some(dt) = value
                        .as_str()
                        .filter(|_| id == "expiry")
                        .and_then(excel_date)
                    {
                        sheet
                            .write_datetime_with_format(n, col, &dt, &date)
                            .context("写入到期时间失败")?;
                        continue;
                    }
                    // Always explicit strings: a client-supplied '=...', '+' or '@' is cell text.
                    sheet
                        .write_string_with_format(n, col, s, &text)
                        .context("写入单元格失败")?;
                }
            }
        }
    }
    sheet
        .autofilter(0, 0, row_count, input.columns.len() as u16 - 1)
        .context("设置表头筛选失败")?;
    let mut file = tempfile::tempfile().context("创建 Excel 临时文件失败")?;
    workbook
        .save_to_writer(&mut file)
        .context("写入 Excel 工作簿失败")?;
    file.flush().context("完成 Excel 文件失败")?;
    file.rewind().context("准备 Excel 文件失败")?;
    Ok(file)
}

pub async fn export(Json(input): Json<ExportRows>) -> ApiResult<Response> {
    let slot = BUILD_SLOTS.try_acquire().map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "其他导出正在进行，请稍后重试".into(),
        )
    })?;
    let file = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        build(input)
    })
    .await
    .context("Excel 生成任务中断")??;
    let size = file.metadata().context("读取 Excel 文件大小失败")?.len();
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    Ok(Response::builder()
        .header(header::CONTENT_TYPE,MIME)
        .header(header::CACHE_CONTROL,"no-store")
        .header(header::CONTENT_LENGTH,size)
        .header(header::CONTENT_DISPOSITION,format!("attachment; filename=\"nodes-{timestamp}.xlsx\"; filename*=UTF-8''%E8%8A%82%E7%82%B9%E6%B8%85%E5%8D%95-{timestamp}.xlsx"))
        .body(Body::from_stream(ReaderStream::new(tokio::fs::File::from_std(file))))
        .context("生成 Excel 下载响应失败")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Read};
    use zip::ZipArchive;

    #[tokio::test]
    async fn workbook_preserves_cell_types_and_embeds_qr_on_its_selected_row() {
        let link = "vless://270d0481-8ac2-439c-b3e8-930b77988ec0@vpn.example:443?type=tcp&security=reality#节点";
        let input = serde_json::from_value(json!({
            "columns":["name","link","expiry","qr","port","note"],
            "rows":[{"name":"=SUM(1,1)","link":link,"expiry":"2028-02-29T16:05:09","qr":link,"port":"443","note":"空 格 & < >"},
                {"name":"次行","expiry":"首次使用后 10 天"},
                {"name":"超出范围","expiry":"+012000-12-31T12:00:00"}]
        })).unwrap();
        let response = export(Json(input))
            .await
            .unwrap_or_else(|e| panic!("{}", e.1));
        assert_eq!(response.headers()[header::CONTENT_TYPE], MIME);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let xml = |zip: &mut ZipArchive<_>, path: &str| {
            let mut xml = String::new();
            zip.by_name(path).unwrap().read_to_string(&mut xml).unwrap();
            xml
        };
        let sheet = xml(&mut zip, "xl/worksheets/sheet1.xml");
        let strings = xml(&mut zip, "xl/sharedStrings.xml");
        let drawing = xml(&mut zip, "xl/drawings/drawing1.xml");
        let styles = xml(&mut zip, "xl/styles.xml");
        assert!(sheet.contains("ref=\"A1:F4\"") && sheet.contains("state=\"frozen\""));
        assert!(sheet.contains("<c r=\"C2\"") && sheet.contains("<v>443</v>"));
        assert!(!sheet.contains("r=\"D2\"") && !sheet.contains("<f>"));
        assert!(strings.contains("首次使用后 10 天") && strings.contains("+012000-12-31T12:00:00"));
        assert!(strings.contains("=SUM(1,1)") && strings.contains(&link.replace('&', "&amp;")));
        assert!(styles.contains("yyyy-mm-dd hh:mm:ss"));
        assert!(styles.contains("wrapText=\"1\""));
        assert!(
            drawing.contains("<xdr:row>1</xdr:row>") && drawing.contains("<xdr:col>3</xdr:col>")
        );
        let mut signature = [0u8; 8];
        zip.by_name("xl/media/image1.png")
            .unwrap()
            .read_exact(&mut signature)
            .unwrap();
        assert_eq!(&signature, b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn export_rejects_unknown_fields_invalid_ports_and_mismatched_images() {
        for (cols, row) in [
            (json!(["name", "name"]), json!({"name":"重复"})),
            (json!(["secret"]), json!({"secret":"x"})),
            (json!(["port"]), json!({"port":65536})),
            (json!(["link"]), json!({"link":"https://wrong.example"})),
            (
                json!(["link", "qr"]),
                json!({"link":"vless://first@a.example:443","qr":"vless://second@b.example:443"}),
            ),
        ] {
            let input = serde_json::from_value(json!({"columns":cols,"rows":[row]})).unwrap();
            assert_eq!(validate(&input).unwrap_err().0, StatusCode::BAD_REQUEST);
        }
    }
}
