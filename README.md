# DSBoard - AstroBox v2 插件

[![CI](https://github.com/MCXCC303/dsboard/actions/workflows/ci.yml/badge.svg)](https://github.com/MCXCC303/dsboard/actions/workflows/ci.yml)

## 编译

### 依赖

- Rust 1.97+
- `rustup target add wasm32-wasip2`
- Python 3

### 编译 wasm

```bash
cd wasm
cargo build --release
```

### 打包为 abp 插件

```bash
python3 scripts/build_dist.py --release --package
# 打包结果：
#   dist/dsboard.wasm
#   dist/manifest.json
#   dist/icon.png
#   dist/DSBoard.abp
```

## 安装与使用

1. **安装插件**：在插件中导入`dist/DSBoard.abp`
2. **授权**：首次加载按提示授予 `network`、`device`、`interconnect`、`register_interconnect_recv`、`thirdpartyapp` 权限
3. **连接手环**：在 AstroBox 里连接小米手环
4. **安装手环端快应用**：安装 DSBand 快应用
5. **配置凭据**：打开插件页面并填入
   - API Key：DeepSeek开放平台的任意API Key均可
   - 平台 Token：前往 `platform.deepseek.com`，打开浏览器开发者工具，点击导出后寻找对应的请求头 `Authorization: Bearer ...`
6. 等待数据获取完成并推送

