//! 插件 UI

use crate::astrobox::psys_host::ui::{self, ElementType, Event, FlexDirection};

use crate::{dates, engine, snapshot, state};

// 事件 id 常量
pub const INPUT_API_KEY: &str = "input-api-key";
pub const INPUT_PLATFORM_TOKEN: &str = "input-platform-token";
pub const INPUT_PUSH_PKG: &str = "input-push-pkg";
pub const INPUT_PUSH_INTERVAL: &str = "input-push-interval";
pub const INPUT_BALANCE_INTERVAL: &str = "input-balance-interval";
pub const INPUT_EXPORT_INTERVAL: &str = "input-export-interval";
pub const BTN_SAVE_SETTINGS: &str = "btn-save-settings";
pub const BTN_BALANCE_NOW: &str = "btn-balance-now";
pub const BTN_EXPORT_NOW: &str = "btn-export-now";
pub const BTN_PUSH_NOW: &str = "btn-push-now";
pub const BTN_HOUSEKEEPING: &str = "btn-housekeeping";
pub const BTN_BACKUP: &str = "btn-backup";
pub const BTN_RESTORE: &str = "btn-restore";

fn p(text: &str) -> ui::Element {
    ui::Element::new(ElementType::P, Some(text))
}

/// 输入框上方提示文本:比正文更小的字号(移动端更紧凑)。
fn hint(text: &str) -> ui::Element {
    p(text).size(12).margin(2)
}

/// 大按钮配色:橙色=配置/持久化,绿色=数据拉取,浅蓝=设备动作。
fn button_style(event_id: &str) -> (&'static str, &'static str) {
    match event_id {
        BTN_SAVE_SETTINGS | BTN_BACKUP | BTN_RESTORE => ("#F97316", "#FFFFFF"),
        BTN_BALANCE_NOW | BTN_EXPORT_NOW => ("#22C55E", "#062D1A"),
        BTN_PUSH_NOW | BTN_HOUSEKEEPING => ("#38BDF8", "#082F49"),
        _ => ("#11182C", "#FFFFFF"),
    }
}

/// 全宽大按钮,与输入框同宽
fn btn(label: &str, event_id: &str) -> ui::Element {
    let (bg, fg) = button_style(event_id);
    ui::Element::new(ElementType::Button, Some(label))
        .padding(10)
        .margin(4)
        .radius(10)
        .bg(bg)
        .text_color(fg)
        .width_full()
        .on(Event::Click, event_id)
}

fn input(event_id: &str, value: &str) -> ui::Element {
    ui::Element::new(ElementType::Input, Some(value))
        .padding(8)
        .radius(8)
        .border(1, "#3A4A6B")
        .width_full()
        .on(Event::Input, event_id)
}

// ============================== 卡片 ==============================

/// 设备详情页看板卡片(on_card_render)。
pub fn render_card(card_id: &str) {
    state::lock().card_element_id = Some(card_id.to_string());
    let root = build_card();
    ui::render(card_id, root);
}

fn build_card() -> ui::Element {
    let a = state::lock();
    let snap = snapshot::build_snapshot(&a.data, &a.settings.provider, a.tz_offset_min);

    let connected = a.device_addr.is_some();
    let (badge_text, badge_bg, badge_fg) = if connected {
        ("已连接 · 自动推送中", "#163B2C", "#87E9C6")
    } else {
        ("未连接设备", "#3B2A2A", "#F0B7B7")
    };
    // 旧版 ui 无 Badge 元素,用带背景色的 P 替代
    let badge = ui::Element::new(ElementType::P, Some(badge_text))
        .padding(6)
        .radius(999)
        .bg(badge_bg)
        .text_color(badge_fg);

    let balance_line = match &snap.balance {
        Some(b) => format!(
            "余额 ¥{:.2} (充值 {:.2} / 赠送 {:.2}) · {}",
            b.total,
            b.top_up.unwrap_or(0.0),
            b.granted.unwrap_or(0.0),
            b.currency
        ),
        None => "余额 -- / 接口不可用或未配置".to_string(),
    };

    let cache_line = match snap.cache.hit_rate {
        Some(r) => format!(
            "今日命中率 {:.1}% (命中 {} / 未命中 {}) · {}",
            r * 100.0,
            fmt_tokens(snap.cache.hit_tokens),
            fmt_tokens(snap.cache.miss_tokens),
            snap.cache.date
        ),
        None => format!("今日暂无调用 · {}", snap.cache.date),
    };

    let mut root = ui::Element::new(ElementType::Div, None)
        .flex()
        .flex_direction(FlexDirection::Column)
        .padding(12)
        .child(badge)
        .child(p(&balance_line))
        .child(p(&cache_line));

    for m in &snap.models {
        root = root.child(p(&format!(
            "{} · {} 次 · 输出 {} · ¥{:.2}",
            m.model,
            m.calls,
            fmt_tokens(m.output_tokens),
            m.cost
        )));
    }

    let last_push = match a.last_push_at {
        Some(t) => format!("上次推送 {} 前", fmt_ago(dates::unix_now() - t)),
        None => "尚未推送".to_string(),
    };
    root.child(p(&format!("新鲜度 {} · {last_push}", snap.freshness)))
        .child(p(&a.status))
        .child(btn("立即推送", BTN_PUSH_NOW))
        .child(btn("查余额", BTN_BALANCE_NOW))
        .child(btn("导出导入", BTN_EXPORT_NOW))
}

// ============================== 设置页 ==============================

/// 插件页面设置表单(on_ui_render)。
pub fn render_page(element_id: &str) {
    state::lock().page_element_id = Some(element_id.to_string());
    let root = build_page();
    ui::render(element_id, root);
}

fn build_page() -> ui::Element {
    // 状态行单独取锁计算(内部会取时区偏移),避免在持有 MutexGuard 期间
    // 再次加锁 —— wasm 目标的 std Mutex 对同线程递归加锁直接 assert 并 abort。
    let status_line = {
        let a = state::lock();
        let tz = a.tz_offset_min;
        format!(
            "状态: {} ({})",
            a.status,
            if a.status_at == 0 {
                "-".to_string()
            } else {
                fmt_epoch(a.status_at, tz)
            }
        )
    };

    let a = state::lock();
    let root = ui::Element::new(ElementType::Div, None)
        .flex()
        .flex_direction(FlexDirection::Column)
        .padding(16)
        .child(hint("API Key (api.deepseek.com,余额接口):"))
        .child(input(INPUT_API_KEY, &a.settings.api_key))
        .child(hint(
            "平台 Token (platform.deepseek.com,用量导出,浏览器 F12 复制):",
        ))
        .child(input(INPUT_PLATFORM_TOKEN, &a.settings.platform_token))
        .child(hint("手环快应用包名:"))
        .child(input(INPUT_PUSH_PKG, &a.settings.push_pkg))
        .child(hint("推送间隔(秒,默认 60):"))
        .child(input(
            INPUT_PUSH_INTERVAL,
            &a.settings.push_interval_secs.to_string(),
        ))
        .child(hint("余额轮询间隔(秒,默认 60):"))
        .child(input(
            INPUT_BALANCE_INTERVAL,
            &a.settings.balance_interval_secs.to_string(),
        ))
        .child(hint("自动导出间隔(秒,默认 60,0=禁用自动导出):"))
        .child(input(
            INPUT_EXPORT_INTERVAL,
            &a.settings.export_interval_secs.to_string(),
        ))
        .child(btn("保存设置", BTN_SAVE_SETTINGS))
        .child(btn("立即查余额", BTN_BALANCE_NOW))
        .child(btn("导出并导入", BTN_EXPORT_NOW))
        .child(btn("推送手环", BTN_PUSH_NOW))
        .child(btn("检测设备", BTN_HOUSEKEEPING))
        .child(hint(
            "跨版本持久化(宿主更新插件会清空插件目录,更新前备份、更新后恢复):",
        ))
        .child(btn("备份配置/数据", BTN_BACKUP))
        .child(btn("从备份恢复", BTN_RESTORE))
        .child(p(&status_line));
    drop(a);
    root
}

/// 手动重绘页面与卡片(宿主不会自动刷新,动作执行后调用)。
fn rerender() {
    let (page, card) = {
        let a = state::lock();
        (a.page_element_id.clone(), a.card_element_id.clone())
    };
    if let Some(id) = page {
        ui::render(&id, build_page());
    }
    if let Some(id) = card {
        ui::render(&id, build_card());
    }
}

// ============================== 事件分发 ==============================

/// 宿主传回的事件载荷是 JSON(如 `{"type":"input","value":"aaa","checked":false}`、
/// `{"type":"click","clientX":255,"value":""}`);取 `value` 字段作为输入框实际内容。
fn payload_value(payload: &str) -> String {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.get("value").and_then(|x| x.as_str()).map(String::from))
        .unwrap_or_else(|| payload.to_string())
}

/// on_ui_event 分发。所有事件先打日志,便于确认事件是否送达。
pub async fn handle_ui_event(event_id: &str, event: &Event, payload: &str) {
    tracing::info!("[ui-event] id={event_id} event={event:?} payload={payload}");

    match event {
        Event::Input => {
            // 输入值即时写入内存状态(保存按钮落盘);
            // 输入过程不重绘(避免打断焦点/光标跳位),立即返回。
            let value = payload_value(payload);
            let mut a = state::lock();
            match event_id {
                INPUT_API_KEY => a.settings.api_key = value,
                INPUT_PLATFORM_TOKEN => a.settings.platform_token = value,
                INPUT_PUSH_PKG => a.settings.push_pkg = value,
                INPUT_PUSH_INTERVAL => {
                    if let Ok(v) = value.parse::<u64>() {
                        a.settings.push_interval_secs = v.max(10);
                    }
                }
                INPUT_BALANCE_INTERVAL => {
                    if let Ok(v) = value.parse::<u64>() {
                        a.settings.balance_interval_secs = v.max(30);
                    }
                }
                INPUT_EXPORT_INTERVAL => {
                    // 不设下限:0 = 禁用自动导出(仅手动);与桌面端配置语义一致
                    if let Ok(v) = value.parse::<u64>() {
                        a.settings.export_interval_secs = v;
                    }
                }
                _ => {}
            }
            return;
        }
        Event::Click => match event_id {
            BTN_SAVE_SETTINGS => {
                state::save_settings();
                engine::arm_timers().await;
                state::set_status("设置已保存,定时器已更新");
            }
            BTN_BALANCE_NOW => engine::balance_now().await,
            BTN_EXPORT_NOW => engine::export_now().await,
            BTN_PUSH_NOW => engine::push_now(true).await,
            BTN_HOUSEKEEPING => engine::refresh_device().await,
            BTN_BACKUP => engine::backup_to_file().await,
            BTN_RESTORE => engine::restore_from_file().await,
            other => {
                tracing::warn!("[ui-event] 未识别的点击 id: {other}");
                return;
            }
        },
        _ => return,
    }

    // 动作执行完重绘,让状态行/卡片立即反映结果
    rerender();
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn fmt_ago(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// 本地 HH:MM:SS(时区偏移由调用方传入,本函数不取全局锁)。
fn fmt_epoch(epoch: i64, tz_offset_min: i32) -> String {
    let local = epoch + tz_offset_min as i64 * 60;
    let h = local.div_euclid(3600).rem_euclid(24);
    let m = local.div_euclid(60).rem_euclid(60);
    let s = local.rem_euclid(60);
    format!("{h:02}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_value_extracts_input_text() {
        // 宿主 Input 事件载荷:值在 value 字段
        let p = r#"{"type":"input","value":"sk-abc123","checked":false}"#;
        assert_eq!(payload_value(p), "sk-abc123");
    }

    #[test]
    fn payload_value_falls_back_to_raw() {
        // 非 JSON 或缺少 value 字段时原样返回,不丢失输入
        assert_eq!(payload_value("plain-text"), "plain-text");
        assert_eq!(
            payload_value(r#"{"type":"click","clientX":255,"value":""}"#),
            ""
        );
    }
}
