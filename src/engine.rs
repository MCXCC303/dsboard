//! 引擎:定时器布防、事件分发、设备发现与互联注册、快照推送。
//!
//! 定时器 payload:
//! - `push`        每 push_interval_secs 推送快照
//! - `balance`     每 balance_interval_secs 查余额
//! - `export`      每 export_interval_secs 导出用量 CSV 并导入
//! - `housekeeping` 30s:刷新已连接设备、注册互联接收、刷新时区

use crate::astrobox::psys_host::{device, interconnect, register, thirdpartyapp, timer};
use serde_json::Value;

use crate::state::{self};
use crate::{dates, deepseek, import, snapshot};

/// 设备详情页卡片 id(register-card 用)
pub const CARD_ID: &str = "deepseek-usage-card";
/// 看板卡名
pub const CARD_NAME: &str = "DeepSeek 用量";
/// 快应用互联消息短窗口去重:vela 的 onopen/getReadyState 或宿主事件重复派发
/// 可能让同一动作产生多条消息,窗口内的重复消息只处理第一条。
const INTERCONNECT_DEDUP_SECS: i64 = 3;

/// on_load 中调用(block_on):加载持久化、注册卡片与互联接收、布防定时器。
pub async fn init() {
    state::init_from_disk();

    let off = crate::astrobox::psys_host::os::timezone_offset_minutes().await;
    {
        state::lock().tz_offset_min = off;
        tracing::info!("[init] 宿主时区偏移 {off} 分钟");
    }

    if register::register_card(register::CardType::Element, CARD_ID, CARD_NAME)
        .await
        .is_ok()
    {
        tracing::info!("[init] 已注册设备页卡片 {CARD_ID}");
    } else {
        tracing::warn!("[init] 注册卡片失败(可能未授权)");
    }

    refresh_device().await;
    arm_timers().await;
    // 先取锁计算结果,释放后再写状态
    let status = {
        let a = state::lock();
        match (&a.device_addr, a.recv_registered) {
            (None, _) => "插件已启动:未连接设备",
            (Some(_), false) => {
                "插件已启动:互联接收未注册成功(检查权限,housekeeping 会重试)"
            }
            (Some(_), true) => "插件已启动:等待手环快应用打开并发送消息",
        }
    };
    state::set_status(status);
}

/// 重新布防全部定时器(设置变更后调用)。
pub async fn arm_timers() {
    let (push_ms, balance_ms, export_ms) = {
        let a = state::lock();
        (
            a.settings.push_interval_secs.saturating_mul(1000),
            a.settings.balance_interval_secs.saturating_mul(1000),
            a.settings.export_interval_secs.saturating_mul(1000),
        )
    };

    // 清理旧定时器
    let old: Vec<u64> = state::lock().timer_ids.values().copied().collect();
    for id in old {
        timer::clear_timer(id).await;
    }

    let mut ids = std::collections::BTreeMap::new();
    ids.insert("push".to_string(), timer::set_interval(push_ms, "push").await);
    ids.insert("balance".to_string(), timer::set_interval(balance_ms, "balance").await);
    // 导出间隔 0 = 禁用自动导出(仅手动触发),不再布防该定时器
    if export_ms > 0 {
        ids.insert("export".to_string(), timer::set_interval(export_ms, "export").await);
    }
    ids.insert(
        "housekeeping".to_string(),
        timer::set_interval(30_000, "housekeeping").await,
    );
    state::lock().timer_ids = ids;
    tracing::info!(
        "[timer] push {}s · balance {}s · export {} · housekeeping 30s",
        push_ms / 1000,
        balance_ms / 1000,
        if export_ms > 0 {
            format!("{}s", export_ms / 1000)
        } else {
            "禁用".to_string()
        }
    );
}

/// on_event 分发。
pub async fn handle_event(
    event_type: crate::exports::astrobox::psys_plugin::event::EventType,
    payload: &str,
) {
    use crate::exports::astrobox::psys_plugin::event::EventType;
    match event_type {
        EventType::Timer => {
            // {"timerId":..,"kind":"interval","payload":"push"}
            let which = serde_json::from_str::<Value>(payload)
                .ok()
                .and_then(|v| v.get("payload").and_then(Value::as_str).map(String::from));
            match which.as_deref() {
                Some("push") => push_now(false).await,
                Some("balance") => balance_now().await,
                Some("export") => export_now().await,
                Some("housekeeping") => housekeeping().await,
                other => tracing::debug!("[timer] 未识别的定时器载荷: {other:?}"),
            }
        }
        EventType::InterconnectMessage => {
            // 手环侧快应用有任何活动(打开/请求状态/主动刷新)都立即回一版快照
            handle_interconnect_message(payload).await;
        }
        other => tracing::debug!("[event] {:?} len={}", other, payload.len()),
    }
}

/// 快应用互联消息应答:手环打开快应用/请求刷新时立即强推一版快照
pub async fn handle_interconnect_message(payload: &str) {
    // 短窗口去重:同一动作可能同时触发 vela 的 onopen 与 getReadyState 两条
    // 上行路径(或宿主重复派发),只响应第一条,避免重复强推。
    let now = dates::unix_now();
    let duplicate = {
        let a = state::lock();
        a.last_interconnect_at
            .map(|t| now.saturating_sub(t) < INTERCONNECT_DEDUP_SECS)
            .unwrap_or(false)
    };
    if duplicate {
        tracing::info!(
            "[interconnect] 忽略 {INTERCONNECT_DEDUP_SECS}s 内的重复快应用消息(len={})",
            payload.len()
        );
        return;
    }
    state::lock().last_interconnect_at = Some(now);

    tracing::info!(
        "[interconnect] 收到快应用消息(len={}) 立即强推快照",
        payload.len()
    );
    push_now(true).await;
}

/// 刷新已连接设备;设备变化时重新注册互联接收。
pub async fn refresh_device() {
    let devices = device::get_connected_device_list().await;
    let addr = devices.first().map(|d| d.addr.clone());
    let pkg = state::lock().settings.push_pkg.clone();

    let need_register = {
        let mut a = state::lock();
        if addr != a.device_addr {
            a.recv_registered = false;
            a.device_addr = addr.clone();
        }
        !a.recv_registered && addr.is_some()
    };

    match &addr {
        Some(a) => tracing::info!("[device] 已连接设备: {}({})", devices[0].name, a),
        None => tracing::debug!("[device] 无已连接设备"),
    }

    if need_register && let Some(a) = &addr {
        match register::register_interconnect_recv(a, &pkg).await {
            Ok(()) => {
                state::lock().recv_registered = true;
                tracing::info!(
                    "[interconnect] 已注册互联接收: {a} {pkg},等待快应用上行消息"
                );
            }
            Err(()) => {
                tracing::warn!(
                    "[interconnect] 注册互联接收失败(检查 register_interconnect_recv 权限);手环快应用的上行消息将无法到达插件"
                );
            }
        }
    }
}

async fn housekeeping() {
    refresh_device().await;
    let off = crate::astrobox::psys_host::os::timezone_offset_minutes().await;
    state::lock().tz_offset_min = off;
}

/// 构建并推送快照到手环快应用。
///
/// `force`:
/// - `false`(定时推送):快照业务数据与上次成功推送相同时跳过;
/// - `true`(手环打开应用 / 设置页手动推送):忽略变化检测,立即回复。
///
/// 推送前通过 `thirdpartyapp` 查询目标快应用是否已安装;确认未安装时
/// 直接跳过并给出明确状态,不再发送必然失败的消息。
pub async fn push_now(force: bool) {
    let (addr, pkg, json, signature) = {
        let a = state::lock();
        let snap = snapshot::build_snapshot(&a.data, &a.settings.provider, a.tz_offset_min);
        let signature = snapshot::stable_signature(&snap);
        let json = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
        (
            a.device_addr.clone(),
            a.settings.push_pkg.clone(),
            json,
            signature,
        )
    };

    let Some(addr) = addr else {
        state::set_status("未连接设备,跳过推送(请先在 AstroBox 连接手环)");
        return;
    };

    // 定时推送做变化检测:同一设备 + 同一业务数据 → 跳过。
    // 手环打开应用/手动按钮走 force=true,始终回复。
    if !force {
        let unchanged = {
            let a = state::lock();
            a.last_pushed_device.as_deref() == Some(addr.as_str())
                && a.last_pushed_signature.as_deref() == Some(signature.as_str())
        };
        if unchanged {
            tracing::info!(
                "[push] 快照无变化({} 字节),跳过推送",
                json.len()
            );
            state::set_status(&format!("快照无变化,跳过推送({} 字节)", json.len()));
            return;
        }
    }

    // 推送前检测:确认手环端已安装目标快应用。
    // 查询失败(权限/设备无响应)时降级为继续推送,不阻断原有链路。
    match thirdpartyapp::get_thirdparty_app_list(&addr).await {
        Ok(apps) => match apps.iter().find(|app| app.package_name == pkg) {
            Some(app) => tracing::info!(
                "[precheck] 目标应用已安装: {} version_code={} app_name={}",
                app.package_name,
                app.version_code,
                app.app_name
            ),
            None => {
                let installed: Vec<&str> = apps.iter().map(|a| a.package_name.as_str()).collect();
                tracing::warn!(
                    "[precheck] 设备 {addr} 未安装 {pkg};已安装快应用: {installed:?}"
                );
                state::set_status(&format!(
                    "推送失败:手环未安装 {pkg},请先通过 AstroBox 安装 vela 快应用"
                ));
                return;
            }
        },
        Err(()) => {
            tracing::warn!(
                "[precheck] 无法获取第三方应用列表(检查 thirdpartyapp 权限/设备响应),跳过检测继续推送"
            );
        }
    }

    match interconnect::send_qaic_message(&addr, &pkg, &json).await {
        Ok(()) => {
            let t = dates::unix_now();
            {
                let mut a = state::lock();
                a.last_push_at = Some(t);
                a.last_pushed_signature = Some(signature);
                a.last_pushed_device = Some(addr.clone());
            }
            tracing::info!("[push] 已发送快照 {} 字节 → {addr} {pkg}", json.len());
            state::set_status(&format!("已推送快照 {} 字节 → {pkg}", json.len()));
        }
        Err(()) => {
            tracing::warn!("[push] send_qaic_message 失败: {addr} {pkg}");
            state::set_status("推送失败:设备不在线/未安装快应用/未授权 interconnect");
        }
    }
}

/// 把当前 settings + data 通过宿主保存对话框导出为备份 JSON。
pub async fn backup_to_file() {
    use crate::astrobox::psys_host::dialog;

    let bytes = match crate::backup::encode_backup() {
        Ok(b) => b,
        Err(e) => {
            state::set_status(&format!("备份失败: {e}"));
            return;
        }
    };

    let filter = dialog::FilterConfig {
        multiple: false,
        extensions: vec!["json".to_string()],
        default_directory: String::new(),
        default_file_name: format!("deepseek-miband-backup-{}.json", dates::unix_now()),
    };
    let Ok(session) = dialog::save_file_start(&filter).await else {
        state::set_status("备份已取消或无法打开保存窗口");
        return;
    };

    for chunk in bytes.chunks(64 * 1024) {
        if let Err(()) = dialog::save_file_write_chunk(session.session_id, chunk).await {
            let _ = dialog::save_file_abort(session.session_id).await;
            state::set_status("备份失败:写入文件出错");
            return;
        }
    }
    match dialog::save_file_finish(session.session_id).await {
        Ok(()) => {
            tracing::info!("[backup] 已导出备份 {} 字节 → {}", bytes.len(), session.name);
            state::set_status(&format!(
                "备份完成: {} 字节 · 更新插件后请用“从备份恢复”导入",
                bytes.len()
            ));
        }
        Err(()) => {
            let _ = dialog::save_file_abort(session.session_id).await;
            state::set_status("备份失败:无法完成保存");
        }
    }
}

/// 通过宿主文件选择框挑选备份 JSON 并恢复 settings + data。
pub async fn restore_from_file() {
    use crate::astrobox::psys_host::dialog;

    let config = dialog::PickConfig {
        read: true,
        copy_to: None,
    };
    let filter = dialog::FilterConfig {
        multiple: false,
        extensions: vec!["json".to_string()],
        default_directory: String::new(),
        default_file_name: String::new(),
    };
    let picked = dialog::pick_file(&config, &filter).await;
    if picked.name.is_empty() && picked.data.is_empty() {
        state::set_status("未选择备份文件(已取消)");
        return;
    }
    if picked.data.is_empty() {
        state::set_status("读取备份文件失败(文件为空或无法读取)");
        return;
    }

    match crate::backup::apply_backup(&picked.data) {
        Ok(summary) => {
            tracing::info!("[backup] 已从 {} 恢复({summary})", picked.name);
            arm_timers().await;
            state::set_status(&format!("{summary}(来自 {})", picked.name));
        }
        Err(e) => state::set_status(&format!("恢复失败: {e}")),
    }
}

pub async fn balance_now() {
    let (base, key) = {
        let a = state::lock();
        (
            a.settings.base_url.clone(),
            state::sanitize_legacy_value(&a.settings.api_key),
        )
    };
    if key.is_empty() {
        state::set_status("未设置 API Key(插件页面输入后保存)");
        return;
    }
    let now = dates::unix_now();
    match deepseek::fetch_balance(&base, &key, now) {
        Ok(Some(info)) => {
            let total = info.total;
            state::lock().data.balance = Some(info);
            state::save_data();
            state::set_status(&format!("余额已更新: ¥{total:.2}"));
        }
        Ok(None) => state::set_status("余额接口不可用(检查 base_url / API Key)"),
        Err(e) => state::set_status(&format!("余额查询失败: {e:#}")),
    }
}

pub async fn export_now() {
    let (platform, token, tz) = {
        let a = state::lock();
        (
            a.settings.platform_base.clone(),
            state::sanitize_legacy_value(&a.settings.platform_token),
            a.tz_offset_min,
        )
    };
    if token.is_empty() {
        state::set_status("未设置平台 token(浏览器 F12 复制,插件页面输入)");
        return;
    }
    let (start, end) = deepseek::default_window(30, tz);
    match deepseek::fetch_export_zip(&platform, &token, start, end) {
        Ok(bytes) => match import::import_zip_bytes(&bytes, tz) {
            Ok((days, models, replaced)) => {
                let now = dates::unix_now();
                state::lock().data.last_import_at = Some(now);
                state::save_data();
                state::set_status(&format!("用量导入成功: {days} 天 · {models} 模型 · 替换 {replaced} 行"));
            }
            Err(e) => state::set_status(&format!("CSV 导入失败: {e}")),
        },
        Err(e) => state::set_status(&format!("导出失败: {e:#}")),
    }
}
