//! 配置/数据备份与恢复
//!
//! 宿主更新插件时会整体删除插件目录,因此插件目录里的 `settings.json` / `data.json` 无法跨版本保留
//! 通过宿主 `dialog.save-file-*` / `dialog.pick-file` 让用户把配置与用量数据导出到自选文件,更新插件后再从该文件恢复。

use serde::{Deserialize, Serialize};

use crate::state::{self, DataFile, Settings};

pub const BACKUP_KIND: &str = "dsboard-backup";
pub const BACKUP_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFile {
    pub kind: String,
    pub version: u32,
    pub exported_at: i64,
    pub settings: Settings,
    pub data: DataFile,
}

/// 把当前 settings + data 序列化为备份 JSON。
pub fn encode_backup() -> Result<Vec<u8>, String> {
    let (settings, data) = {
        let a = state::lock();
        (a.settings.clone(), a.data.clone())
    };
    let file = BackupFile {
        kind: BACKUP_KIND.into(),
        version: BACKUP_VERSION,
        exported_at: crate::dates::unix_now(),
        settings,
        data,
    };
    serde_json::to_vec_pretty(&file).map_err(|e| format!("序列化备份失败: {e}"))
}

/// 解析备份文件并覆盖当前 settings + data。
/// 返回给用户看的摘要字符串。
pub fn apply_backup(bytes: &[u8]) -> Result<String, String> {
    let file: BackupFile =
        serde_json::from_slice(bytes).map_err(|e| format!("备份文件不是有效 JSON: {e}"))?;
    if file.kind != BACKUP_KIND {
        return Err(format!(
            "不是本插件的备份文件(kind={})",
            file.kind
        ));
    }
    if file.version != BACKUP_VERSION {
        return Err(format!(
            "备份版本不受支持: {} (当前支持 {BACKUP_VERSION})",
            file.version
        ));
    }

    {
        let mut a = state::lock();
        let mut settings = file.settings;
        // 恢复旧备份时同步迁移 0.1.x 的默认包名
        if settings.push_pkg == state::LEGACY_DEFAULT_PKG {
            settings.push_pkg = state::DEFAULT_PKG.into();
        }
        a.settings = settings;
        a.data = file.data;
        // 恢复后强制下一轮重新推送(签名缓存作废)
        a.last_pushed_signature = None;
        a.last_pushed_device = None;
    }
    state::prune();
    state::save_settings();
    state::save_data();

    let (days, has_balance) = {
        let a = state::lock();
        (a.data.days.len(), a.data.balance.is_some())
    };
    Ok(format!(
        "备份已恢复: {days} 天用量 · 余额{}",
        if has_balance { "有" } else { "无" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_file_roundtrip_keeps_settings_and_data() {
        let file = BackupFile {
            kind: BACKUP_KIND.into(),
            version: BACKUP_VERSION,
            exported_at: 1_754_000_000,
            settings: Settings::default(),
            data: DataFile::default(),
        };
        let bytes = serde_json::to_vec(&file).unwrap();
        let parsed: BackupFile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.kind, BACKUP_KIND);
        assert_eq!(parsed.version, BACKUP_VERSION);
        assert_eq!(parsed.settings.push_interval_secs, 60);
        assert!(parsed.data.days.is_empty());
    }
}
