//! DeepSeek HTTP 接口(wasi-http via waki,同步阻塞式,勿在 UI 渲染路径调用)。
//!
//! - 余额:`GET {base}/user/balance`(Bearer API Key)
//! - 用量导出:`GET {platform}/api/v0/usage/export?start=&end=&tz=`(Bearer 平台 token)

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use waki::Client;

use crate::snapshot::BalanceInfo;

pub const BALANCE_PATH: &str = "/user/balance";
pub const EXPORT_PATH: &str = "/api/v0/usage/export";
/// 平台时区 UTC+8
pub const TZ_SEC: i64 = 28_800;
/// 缺省 UA 会被平台反爬拦截(429),与桌面端一致
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:152.0) Gecko/20100101 Firefox/152.0";

/// 失败返回 Ok(None)(接口不可用),不拖垮整体链路;网络/解析错误返回 Err。
pub fn fetch_balance(base_url: &str, api_key: &str, checked_at: i64) -> Result<Option<BalanceInfo>> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), BALANCE_PATH);
    let resp = Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .connect_timeout(Duration::from_secs(10))
        .send()
        .map_err(|e| anyhow!("余额请求失败: {url} ({e})"))?;

    let status = resp.status_code();
    if status != 200 {
        // 非 200 一律按“接口不可用”处理,但把状态码、可读提示与响应体
        // 预览全部打进宿主日志,便于区分 401/402/403/404/429/5xx。
        let raw = resp.body().unwrap_or_default();
        let preview = String::from_utf8_lossy(&raw[..raw.len().min(300)]);
        tracing::warn!(
            "[balance] HTTP {status} {} url={url} body-preview={preview}",
            balance_http_hint(status)
        );
        return Ok(None);
    }
    let body = resp.body().context("读取余额响应体失败")?;
    tracing::debug!("[balance] HTTP 200 url={url} bytes={}", body.len());
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("[balance] 响应非 JSON: {e}");
            return Ok(None);
        }
    };
    Ok(parse_balance_body(&v, checked_at))
}

/// 余额接口非 200 状态码的可读提示(仅日志用)。
fn balance_http_hint(status: u16) -> &'static str {
    match status {
        401 => "(401 未授权:API Key 无效/过期)",
        402 => "(402 余额不足/欠费)",
        403 => "(403 禁止访问:检查 key 权限或 IP)",
        404 => "(404 接口不存在:检查 base_url)",
        429 => "(429 请求过频/限流,稍后重试)",
        s if (500..600).contains(&s) => "(服务端错误,稍后重试)",
        _ => "",
    }
}

/// 字段值可能为字符串或数字
/// ```json
/// {
///   "is_available": true,
///   "balance_infos": [{
///     "currency": "CNY",
///     "total_balance": "128.47",
///     "topped_up_balance": "118.47",
///     "granted_balance": "10.00"
///   }]
/// }
/// ```
fn parse_balance_body(body: &Value, checked_at: i64) -> Option<BalanceInfo> {
    if body.get("is_available").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let infos = body.get("balance_infos")?.as_array()?;
    let info = infos
        .iter()
        .find(|i| i.get("currency").and_then(Value::as_str) == Some("CNY"))
        .or_else(|| infos.first())?;

    let total = as_f64(info.get("total_balance")?)?;
    let currency = info
        .get("currency")
        .and_then(Value::as_str)
        .unwrap_or("CNY")
        .to_string();
    Some(BalanceInfo {
        total,
        top_up: info.get("topped_up_balance").and_then(as_f64),
        granted: info.get("granted_balance").and_then(as_f64),
        currency,
        checked_at,
    })
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// 下载用量导出 zip。非 zip 响应(错误 JSON)转成带错误码提示的 Err。
pub fn fetch_export_zip(platform_base: &str, token: &str, start_sec: i64, end_sec: i64) -> Result<Vec<u8>> {
    let url = format!(
        "{}{}?start={start_sec}&end={end_sec}&tz={TZ_SEC}",
        platform_base.trim_end_matches('/'),
        EXPORT_PATH
    );
    let resp = Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", USER_AGENT)
        .header("x-client-bundle-id", "com.deepseek.chat")
        .header("x-client-platform", "web")
        .header("x-client-version", "1.0.0")
        .header("x-client-locale", "zh_CN")
        .header("x-client-timezone-offset", TZ_SEC.to_string())
        .connect_timeout(Duration::from_secs(60))
        .send()
        .map_err(|e| anyhow!("导出请求失败: {url} ({e})"))?;

    let status = resp.status_code();
    let content_type = resp
        .header("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let raw = resp.body().context("读取导出响应体失败")?;

    if !content_type.contains("zip") || status != 200 {
        // 非 zip 响应(错误 JSON/HTML/空体),把可诊断信息都带上:
        let body: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
        let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
        // 平台业务错误封装在 data.biz_code / data.biz_msg(如 INVALID_PARAM)
        let biz_msg = body
            .pointer("/data/biz_msg")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let biz_code = body
            .pointer("/data/biz_code")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let msg = biz_msg
            .or_else(|| body.get("msg").and_then(Value::as_str).filter(|s| !s.is_empty()))
            .or_else(|| {
                body.get("error")
                    .and_then(|e| e.get("message").or(Some(e)))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("未知错误")
            .to_string();
        let preview = String::from_utf8_lossy(&raw[..raw.len().min(300)]);
        let hint = export_error_hint(status, code, biz_code);
        tracing::warn!(
            "[export] HTTP {status} content-type={content_type:?} url={url} \
             code={code} biz_code={biz_code} msg={msg} body-preview={preview} {hint}"
        );
        anyhow::bail!("平台返回错误: {msg} (code={code}, biz={biz_code}, HTTP {status}) {hint}");
    }

    tracing::info!(
        "[export] HTTP 200 content-type={content_type:?} url={url} bytes={}",
        raw.len()
    );
    Ok(raw)
}

fn export_error_hint(status: u16, code: i64, biz_code: i64) -> &'static str {
    match (status, code, biz_code) {
        (401, _, _) | (_, 40003, _) => "token 无效或已过期,请重新从浏览器 F12 复制",
        (_, 40002, _) => "缺少 token",
        (_, 40029, _) => "IP 访问受限(40029),请更换网络后重试",
        (429, _, _) | (_, 429, _) => "请求过于频繁或触发反爬,稍后重试",
        (_, _, 1) => "INVALID_PARAM:start/end 必须为本地 0 点对齐的整点边界(已自动对齐,若仍报错请反馈)",
        (403, _, _) => "HTTP 403:访问被拒绝,检查 token 权限/网络环境",
        (404, _, _) => "HTTP 404:导出接口不存在,检查 platform_base 配置",
        (s, _, _) if s >= 500 => "平台服务端错误,稍后重试",
        _ => "",
    }
}

/// 默认导出窗口:最近 `days` 天。
///
/// 平台校验 start/end 必须为**整点边界**,end 为**次日 0 点**(排除式,与桌面端一致):
/// - end = 本地时区"明天 0 点"的 Unix 秒;
/// - start = 本地时区"(今天 - days + 1) 0 点"的 Unix 秒。
pub fn default_window(days: i64, tz_offset_min: i32) -> (i64, i64) {
    let now = crate::dates::unix_now();
    // 本地时区今天的"日序"(自 epoch 的天数)
    let today_days = (now + tz_offset_min as i64 * 60).div_euclid(86_400);
    let local_midnight = |days: i64| days * 86_400 - tz_offset_min as i64 * 60;
    let start = local_midnight(today_days - (days - 1));
    let end = local_midnight(today_days + 1);
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_window_aligns_to_local_midnight() {
        // 宿主时区 +08:00:本地 0 点 = epoch 秒 + 28800 后能被 86400 整除
        let (start, end) = default_window(30, 480);
        assert_eq!((start + 28_800).rem_euclid(86_400), 0, "start 须为本地 0 点");
        assert_eq!((end + 28_800).rem_euclid(86_400), 0, "end 须为本地 0 点");
        // end 为次日 0 点,窗口覆盖 30 个自然日
        assert_eq!(end - start, 30 * 86_400);
        assert!(start < crate::dates::unix_now() && crate::dates::unix_now() < end);
    }

    #[test]
    fn default_window_handles_other_timezones() {
        let (start, end) = default_window(7, 0); // UTC
        assert_eq!(start.rem_euclid(86_400), 0);
        assert_eq!(end.rem_euclid(86_400), 0);
        assert_eq!(end - start, 7 * 86_400);
    }

    #[test]
    fn export_error_hint_covers_known_platform_codes() {
        assert!(export_error_hint(200, 40002, -1).contains("缺少 token"));
        assert!(export_error_hint(200, 40003, -1).contains("token 无效或已过期"));
        assert!(export_error_hint(401, -1, -1).contains("token 无效或已过期"));
        assert!(export_error_hint(200, 40029, -1).contains("IP 访问受限"));
        assert!(export_error_hint(200, -1, 1).contains("INVALID_PARAM"));
        assert!(export_error_hint(403, -1, -1).contains("403"));
        assert!(export_error_hint(404, -1, -1).contains("404"));
        assert!(export_error_hint(503, -1, -1).contains("服务端错误"));
        assert!(export_error_hint(429, -1, -1).contains("频繁"));
        assert_eq!(export_error_hint(418, -1, -1), "");
    }

    #[test]
    fn balance_http_hint_covers_common_statuses() {
        assert!(balance_http_hint(401).contains("401"));
        assert!(balance_http_hint(402).contains("余额不足"));
        assert!(balance_http_hint(403).contains("403"));
        assert!(balance_http_hint(404).contains("404"));
        assert!(balance_http_hint(429).contains("429"));
        assert!(balance_http_hint(503).contains("服务端错误"));
        assert_eq!(balance_http_hint(200), "");
    }
}
