//! 用量 CSV 导入

use std::collections::BTreeMap;
use std::io::Cursor;

use thiserror::Error;

use crate::dates;
use crate::state::{self, DayModelUsage};

pub const AMOUNT_HEADER: &[&str] = &[
    "user_id", "start_time_iso", "end_time_iso", "model", "api_key_name",
    "api_key", "type", "price", "amount",
];
pub const COST_HEADER: &[&str] = &[
    "user_id", "start_time_iso", "end_time_iso", "model", "wallet_type", "cost", "currency",
];

pub const TYPE_HIT: &str = "input_cache_hit_tokens";
pub const TYPE_MISS: &str = "input_cache_miss_tokens";
pub const TYPE_OUTPUT: &str = "output_tokens";
pub const TYPE_CALLS: &str = "request_count";

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("zip 解析失败: {0}")]
    Zip(String),
    #[error("缺少文件: {0}(zip 中未找到)")]
    MissingCsv(&'static str),
    #[error("{0} 表头不符合预期: {1}")]
    BadHeader(&'static str, String),
    #[error("{0} 第 {1} 行: {2}")]
    BadLine(&'static str, u64, String),
    #[error("未知指标类型: {0}(平台格式可能已变更)")]
    UnknownType(String),
    #[error("文件编码非 UTF-8")]
    Encoding,
    #[error("CSV 无有效数据行")]
    EmptyData,
}

struct AmountLine {
    date: String,
    model: String,
    type_: String,
    amount: u64,
}

struct CostLine {
    date: String,
    model: String,
    cost: f64,
}

/// 导入平台导出的用量 zip(内存字节)。
/// 返回 (涉及天数, 模型数, 替换行数)。
pub fn import_zip_bytes(bytes: &[u8], tz_offset_min: i32) -> Result<(usize, usize, usize), ImportError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| ImportError::Zip(e.to_string()))?;

    let mut amount_text: Option<String> = None;
    let mut cost_text: Option<String> = None;
    for i in 0..archive.len() {
        let name = archive
            .by_index(i)
            .map(|f| f.name().to_string())
            .map_err(|e| ImportError::Zip(e.to_string()))?;
        if name.ends_with(".csv") {
            if name.starts_with("amount-") && amount_text.is_none() {
                amount_text = Some(read_entry_text(&mut archive, i)?);
            } else if name.starts_with("cost-") && cost_text.is_none() {
                cost_text = Some(read_entry_text(&mut archive, i)?);
            }
        }
    }
    let amount_text = amount_text.ok_or(ImportError::MissingCsv("amount-*.csv"))?;
    let cost_text = cost_text.ok_or(ImportError::MissingCsv("cost-*.csv"))?;

    let amount_rows = parse_amount_csv(&amount_text, tz_offset_min)?;
    let cost_rows = parse_cost_csv(&cost_text, tz_offset_min)?;
    let agg = aggregate(amount_rows, cost_rows)?;
    if agg.is_empty() {
        return Err(ImportError::EmptyData);
    }

    let mut replaced = 0usize;
    let mut days = BTreeMap::<String, ()>::new();
    let mut models = BTreeMap::<String, ()>::new();
    for ((date, model), row) in agg {
        if state::upsert_daily(&date, row) {
            replaced += 1;
        }
        days.insert(date, ());
        models.insert(model, ());
    }
    state::prune();
    state::save_data();
    Ok((days.len(), models.len(), replaced))
}

fn read_entry_text(archive: &mut zip::ZipArchive<Cursor<&[u8]>>, i: usize) -> Result<String, ImportError> {
    let mut entry = archive
        .by_index(i)
        .map_err(|e| ImportError::Zip(e.to_string()))?;
    let mut buf = Vec::new();
    use std::io::Read;
    entry
        .read_to_end(&mut buf)
        .map_err(|e| ImportError::Zip(e.to_string()))?;
    String::from_utf8(buf).map_err(|_| ImportError::Encoding)
}

fn parse_amount_csv(text: &str, tz_offset_min: i32) -> Result<Vec<AmountLine>, ImportError> {
    let text = strip_bom(text);
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    validate_header(&mut reader, "amount", AMOUNT_HEADER)?;

    let mut rows = Vec::new();
    for result in reader.records() {
        let rec = result.map_err(|e| line_err("amount", &e))?;
        let line = rec.position().map(|p| p.line()).unwrap_or(0);
        if rec.iter().all(|f| f.is_empty()) {
            continue;
        }
        let date = parse_date(&rec[1], tz_offset_min).map_err(|e| row_err("amount", line, &e))?;
        let model = rec.get(3).unwrap_or_default().to_string();
        let type_ = rec.get(6).unwrap_or_default().to_string();
        if ![TYPE_HIT, TYPE_MISS, TYPE_OUTPUT, TYPE_CALLS].contains(&type_.as_str()) {
            return Err(ImportError::UnknownType(type_));
        }
        let price = rec.get(7).unwrap_or_default();
        if !price.is_empty() && price.parse::<f64>().is_err() {
            return Err(row_err("amount", line, &format!("price 非数字: {price}")));
        }
        let amount = rec
            .get(8)
            .unwrap_or_default()
            .parse::<u64>()
            .map_err(|_| row_err("amount", line, &format!("amount 非非负整数: {}", rec.get(8).unwrap_or_default())))?;
        rows.push(AmountLine { date, model, type_, amount });
    }
    Ok(rows)
}

fn parse_cost_csv(text: &str, tz_offset_min: i32) -> Result<Vec<CostLine>, ImportError> {
    let text = strip_bom(text);
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    validate_header(&mut reader, "cost", COST_HEADER)?;

    let mut rows = Vec::new();
    for result in reader.records() {
        let rec = result.map_err(|e| line_err("cost", &e))?;
        let line = rec.position().map(|p| p.line()).unwrap_or(0);
        if rec.iter().all(|f| f.is_empty()) {
            continue;
        }
        let date = parse_date(&rec[1], tz_offset_min).map_err(|e| row_err("cost", line, &e))?;
        let model = rec.get(3).unwrap_or_default().to_string();
        let wallet = rec.get(4).unwrap_or_default().to_string();
        if !["Paid", "Granted"].contains(&wallet.as_str()) {
            return Err(row_err("cost", line, &format!("未知 wallet_type: {wallet}")));
        }
        let cost = rec
            .get(5)
            .unwrap_or_default()
            .parse::<f64>()
            .map_err(|_| row_err("cost", line, &format!("cost 非数字: {}", rec.get(5).unwrap_or_default())))?;
        rows.push(CostLine { date, model, cost });
    }
    Ok(rows)
}

fn validate_header(
    reader: &mut csv::Reader<&[u8]>,
    which: &'static str,
    expected: &[&str],
) -> Result<(), ImportError> {
    let headers = reader.headers().map_err(|e| line_err(which, &e))?;
    let actual: Vec<&str> = headers.iter().collect();
    if actual.len() != expected.len()
        || actual.iter().zip(expected).any(|(a, e)| a != e)
    {
        return Err(ImportError::BadHeader(
            which,
            format!("期望 {} 列,实际 {} 列: {:?}", expected.len(), actual.len(), actual),
        ));
    }
    Ok(())
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// 平台为 +08:00,换算到采集端本地时区。
fn parse_date(s: &str, tz_offset_min: i32) -> std::result::Result<String, String> {
    dates::local_date_from_rfc3339(s, tz_offset_min)
        .ok_or_else(|| format!("日期格式非法: {s}"))
}

/// 补零初始化所有 (date, model) 组合,防止平台省略"命中为 0"的行时高估命中率。
fn aggregate(
    amount_rows: Vec<AmountLine>,
    cost_rows: Vec<CostLine>,
) -> Result<BTreeMap<(String, String), DayModelUsage>, ImportError> {
    let mut map: BTreeMap<(String, String), DayModelUsage> = BTreeMap::new();
    for r in amount_rows {
        let e = entry(&mut map, &r.date, &r.model);
        match r.type_.as_str() {
            TYPE_HIT => e.hit_tokens += r.amount,
            TYPE_MISS => e.miss_tokens += r.amount,
            TYPE_OUTPUT => e.output_tokens += r.amount,
            TYPE_CALLS => e.calls += r.amount,
            _ => unreachable!("parse_amount_csv 已校验 type"),
        }
    }
    for r in cost_rows {
        let e = entry(&mut map, &r.date, &r.model);
        e.cost += r.cost;
    }
    Ok(map)
}

fn entry<'a>(
    map: &'a mut BTreeMap<(String, String), DayModelUsage>,
    date: &str,
    model: &str,
) -> &'a mut DayModelUsage {
    map.entry((date.to_string(), model.to_string()))
        .or_insert_with(|| DayModelUsage {
            model: model.to_string(),
            calls: 0,
            hit_tokens: 0,
            miss_tokens: 0,
            output_tokens: 0,
            cost: 0.0,
        })
}

fn line_err(which: &'static str, e: &csv::Error) -> ImportError {
    let line = e.position().map(|p| p.line()).unwrap_or(0);
    row_err(which, line, &e.to_string())
}

fn row_err(which: &'static str, line: u64, detail: &str) -> ImportError {
    ImportError::BadLine(which, line, detail.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AMOUNT_SAMPLE: &str = "\u{feff}user_id,start_time_iso,end_time_iso,model,api_key_name,api_key,type,price,amount\n00000000-0000-0000-0000-000000000000,2026-07-13T00:00:00+08:00,2026-07-14T00:00:00+08:00,deepseek-v4-pro,key-a,sk-******example******,input_cache_hit_tokens,0.000000025,105402624\n00000000-0000-0000-0000-000000000000,2026-07-13T00:00:00+08:00,2026-07-14T00:00:00+08:00,deepseek-v4-pro,key-a,sk-******example******,input_cache_miss_tokens,0.000003,1885620\n00000000-0000-0000-0000-000000000000,2026-07-13T00:00:00+08:00,2026-07-14T00:00:00+08:00,deepseek-v4-pro,key-a,sk-******example******,request_count,,666\n00000000-0000-0000-0000-000000000000,2026-07-13T00:00:00+08:00,2026-07-14T00:00:00+08:00,deepseek-v4-pro,key-a,sk-******example******,output_tokens,0.000006,352921\n00000000-0000-0000-0000-000000000000,2026-07-13T00:00:00+08:00,2026-07-14T00:00:00+08:00,deepseek-v4-flash,key-b,sk-******example******,request_count,,88\n00000000-0000-0000-0000-000000000000,2026-07-13T00:00:00+08:00,2026-07-14T00:00:00+08:00,deepseek-v4-flash,key-b,sk-******example******,output_tokens,0.000006,20209\n";
    const COST_SAMPLE: &str = "\u{feff}user_id,start_time_iso,end_time_iso,model,wallet_type,cost,currency\n00000000-0000-0000-0000-000000000000,2026-07-13T00:00:00+08:00,2026-07-14T00:00:00+08:00,deepseek-v4-pro,Paid,10.4094516000000000,CNY\n00000000-0000-0000-0000-000000000000,2026-07-13T00:00:00+08:00,2026-07-14T00:00:00+08:00,deepseek-v4-flash,Paid,0.9667603600000000,CNY\n";

    #[test]
    fn aggregate_matches_sample_expectations() {
        // 采集端时区 +08:00
        let amount = parse_amount_csv(AMOUNT_SAMPLE, 480).unwrap();
        let cost = parse_cost_csv(COST_SAMPLE, 480).unwrap();
        let agg = aggregate(amount, cost).unwrap();
        assert_eq!(agg.len(), 2);

        let e = agg
            .get(&("2026-07-13".to_string(), "deepseek-v4-pro".to_string()))
            .expect("组合应存在");
        assert_eq!(e.hit_tokens, 105_402_624);
        assert_eq!(e.miss_tokens, 1_885_620);
        assert_eq!(e.output_tokens, 352_921);
        assert_eq!(e.calls, 666);
        assert_eq!(e.cost, 10.4094516);

        // 补零:flash 未导出的 hit/miss 应为 0
        let e = agg
            .get(&("2026-07-13".to_string(), "deepseek-v4-flash".to_string()))
            .expect("组合应存在");
        assert_eq!(e.hit_tokens, 0);
        assert_eq!(e.miss_tokens, 0);
        assert_eq!(e.calls, 88);
        assert_eq!(e.output_tokens, 20_209);
        assert_eq!(e.cost, 0.96676036);
    }
}
