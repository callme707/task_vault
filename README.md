# TaskVault（任务记录工具）

Windows 桌面工具：全局快捷键快速记录 → 临时数据库（inbox.db）→ DeepSeek AI 自动整理 → 主数据库（tasks.db）正式任务与时间安排。

## 当前已实现（M0 + M1 + M2 基础）
- 开机自启开关（主窗口设置中可开/关，开启后以 `--hidden` 静默启动到托盘）
- 全局快捷键：
  - `Ctrl+Alt+Q`：快速记录窗口呼出 / 隐藏（toggle）
  - `Ctrl+Alt+W`：主窗口呼出 / 隐藏（toggle）
- 托盘图标：打开主窗口、快速记录、退出
- 双 SQLite 数据库：`inbox.db` / `tasks.db`（WAL 模式）
- 快速窗口录入后写入临时库，原始记录只读保留
- 后台自动调用 DeepSeek 官方 API（默认 `deepseek-v4-flash`）做 JSON 结构化整理
- 主窗口：临时库列表 / 原始记录 + AI 解析结果 / 转正草稿 / 主库任务列表
- 临时记录转正式任务（保留来源与原始快照）
- 主库任务操作：手动调整优先级、归档/恢复归档、删除（软删除）

## 目录
```
src-tauri/             Rust 后端（Tauri 2）
  src/db.rs            双 SQLite 数据库
  src/deepseek.rs      DeepSeek Chat Completions 客户端
  src/settings.rs      设置读写
  src/commands.rs      前端可调用的命令
  src/lib.rs           窗口/托盘/快捷键/应用状态
ui/                    静态前端（无 npm 构建）
```

## 运行
```bash
cd src-tauri
cargo run
```

首次启动会创建 `%APPDATA%/com.local.taskvault/`：
- `inbox.db`：临时库
- `tasks.db`：主库
- `settings.json`：配置（API Key、模型等）

在主窗口右上角“设置”中填入 DeepSeek API Key；也可以设置环境变量 `DEEPSEEK_API_KEY`。

## 快捷键使用
1. 按 `Ctrl+Alt+Q` 呼出快速记录窗；
2. 输入任务/需求/想法，`Enter` 保存并隐藏，`Esc` 隐藏；
3. 后台自动解析（不阻塞窗口关闭）；
4. 打开主窗口，查看 AI 解析结果，修改草稿后点“转入主数据库”。

## 打包
```bash
cargo install tauri-cli --locked   # 首次需要安装 tauri-cli
cd src-tauri
cargo tauri build                  # 需要系统安装 NSIS/WiX，目标为 Windows
```

## 后续计划
- 设置页开放开机自启开关、快捷键自定义
- AI 批量整理/合并相似临时记录（不修改原文）
- 主库任务编辑、进度更新、时间线视图
- 每日/每周 AI 排期建议与到期提醒
- 数据库备份与导出
