//! 快照契约
//! 手环端 vela 快应用可直接消费同一 JSON。

use serde::{Deserialize, Serialize};

pub const SNAPSHOT_V: u32 = 1;
/// 契约 maxItems
pub const MAX_MODELS: usize = 4;
/// 新鲜度阈值(秒)
pub const FRESH_SECS: i64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub v: u32,
    pub generated_at: i64,
    pub provider: String,
    /// null 表示余额接口不可用
    pub balance: Option<BalanceInfo>,
    pub cache: CacheSummary,
    pub models: Vec<ModelUsage>,
    /// `current` / `cached` / `unavailable`
    pub freshness: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceInfo {
    pub total: f64,
    pub top_up: Option<f64>,
    pub granted: Option<f64>,
    pub currency: String,
    /// Unix 秒,用于新鲜度判定
    pub checked_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSummary {
    pub date: String,
    /// hit/(hit+miss);当日无调用时为 null
    pub hit_rate: Option<f64>,
    pub hit_tokens: u64,
    pub miss_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    pub calls: u64,
    pub hit_tokens: u64,
    pub miss_tokens: u64,
    pub output_tokens: u64,
    /// 平台 CSV 准数(CNY)
    pub cost: f64,
}

use crate::dates;
use crate::state::DataFile;

/// 聚合快照(与桌面端 aggregator::build_snapshot 等价)。
pub fn build_snapshot(data: &DataFile, provider: &str, tz_offset_min: i32) -> Snapshot {
    let now = dates::unix_now();
    let today_str = dates::today_local(tz_offset_min);

    let mut hit_tokens: u64 = 0;
    let mut miss_tokens: u64 = 0;
    let mut models: Vec<ModelUsage> = Vec::new();
    if let Some(rows) = data.days.get(&today_str) {
        for r in rows {
            hit_tokens += r.hit_tokens;
            miss_tokens += r.miss_tokens;
            models.push(ModelUsage {
                model: r.model.clone(),
                calls: r.calls,
                hit_tokens: r.hit_tokens,
                miss_tokens: r.miss_tokens,
                output_tokens: r.output_tokens,
                cost: r.cost,
            });
        }
    }
    let hit_rate = if hit_tokens + miss_tokens > 0 {
        Some(hit_tokens as f64 / (hit_tokens + miss_tokens) as f64)
    } else {
        None
    };
    models.sort_by_key(|m| std::cmp::Reverse(m.calls));
    models.truncate(MAX_MODELS);

    let freshness = freshness(
        data.balance.as_ref().map(|b| b.checked_at),
        data.last_import_at,
        now,
    );

    Snapshot {
        v: SNAPSHOT_V,
        generated_at: now,
        provider: provider.to_string(),
        balance: data.balance.clone(),
        cache: CacheSummary {
            date: today_str,
            hit_rate,
            hit_tokens,
            miss_tokens,
        },
        models,
        freshness,
    }
}

fn freshness(balance_checked: Option<i64>, last_import: Option<i64>, now: i64) -> &'static str {
    let fresh = |t: Option<i64>| t.map(|t| now - t <= FRESH_SECS).unwrap_or(false);
    if fresh(balance_checked) || fresh(last_import) {
        "current"
    } else if balance_checked.is_some() || last_import.is_some() {
        "cached"
    } else {
        "unavailable"
    }
}

/// 推送变化检测用的稳定签名。
///
/// 只对“业务数据”做序列化签名,排除每次构建都会变化的 `generatedAt` 和由时间派生的 `freshness`
/// 余额刷新(`checkedAt` 变化)、日期滚动、模型/缓存变化都会产生新签名。
pub fn stable_signature(snap: &Snapshot) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Stable<'a> {
        provider: &'a str,
        balance: Option<&'a BalanceInfo>,
        cache: &'a CacheSummary,
        models: &'a [ModelUsage],
    }

    serde_json::to_string(&Stable {
        provider: &snap.provider,
        balance: snap.balance.as_ref(),
        cache: &snap.cache,
        models: &snap.models,
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Snapshot {
        Snapshot {
            v: SNAPSHOT_V,
            generated_at: 1_754_000_000,
            provider: "deepseek".into(),
            balance: Some(BalanceInfo {
                total: 128.47,
                top_up: Some(118.0),
                granted: Some(10.47),
                currency: "CNY".into(),
                checked_at: 1_754_000_000,
            }),
            cache: CacheSummary {
                date: "2026-07-13".into(),
                hit_rate: Some(0.9824),
                hit_tokens: 105_402_624,
                miss_tokens: 1_885_620,
            },
            models: vec![ModelUsage {
                model: "deepseek-v4-pro".into(),
                calls: 666,
                hit_tokens: 105_402_624,
                miss_tokens: 1_885_620,
                output_tokens: 352_921,
                cost: 10.4094516,
            }],
            freshness: "current",
        }
    }

    #[test]
    fn stable_signature_ignores_volatile_fields() {
        let sig = stable_signature(&sample());

        // generatedAt / freshness 是易变字段,不应触发推送
        let mut regenerated = sample();
        regenerated.generated_at += 123;
        regenerated.freshness = "cached";
        assert_eq!(stable_signature(&regenerated), sig);

        // 数据变化必须触发推送
        let mut changed = sample();
        changed.models[0].calls += 1;
        assert_ne!(stable_signature(&changed), sig);

        // 余额刷新(checkedAt 变化)也是变化
        let mut refreshed = sample();
        refreshed.balance.as_mut().unwrap().checked_at += 60;
        assert_ne!(stable_signature(&refreshed), sig);
    }
}
