# TaskVault（任务记录工具）

Windows 桌面任务工作台：快速记录文字或截图，AI 整理成建议，确认后加入正式任务。Rust + Tauri 2，原生 HTML/CSS/JavaScript，无前端构建依赖。

## 使用方式

- `Ctrl+Alt+Q` 呼出/隐藏快速记录；`Enter` 保存，`Shift+Enter` 换行，`Esc` 隐藏。
- `Ctrl+Alt+W` 呼出/隐藏工作台。关闭窗口会驻留托盘，托盘“退出”结束应用。
- 导航分为“记录区”和“正式任务”；选择记录后打开详情与可编辑草稿。
- 正式任务分为“进行中 / 已完成 / 已归档”。未归档的待办、进行中和阻塞任务显示在“进行中”；完成后移至“已完成”。两类任务都能直接归档，恢复时保留原完成状态；已完成任务也可重开。
- 主窗口使用与界面一致的深色标题栏，可拖动、双击最大化，并提供最小化、最大化/还原和关闭到托盘按钮。
- 一张图保留一条原始记录。AI 可识别多个待办，每项分别确认、分别显示是否已添加。
- 任务支持状态、优先级、截止/计划时间、标签、工时、进度和子任务；也可直接新建。
- 状态、进度、工时与子任务完成数各自独立；子任务操作即时保存，关闭编辑窗口不撤销这些操作。

## 可靠性与数据保护

- 任务、子任务和收件箱转正标记统一提交；同一原始待办重复提交返回已有任务。
- AI 请求持久排队，同一条记录不会同时重复解析，最多两条并行。重启后恢复未完成的已请求任务；历史失败不会无条件反复重试。
- PNG/JPEG/GIF/WebP 在保存前统一校验，单图最多 4 MiB；附件缺失或损坏会明确标记解析失败。
- 截断后恢复的 AI 结果标为“待核对”，不能视为完整识别。
- 时间统一以带时区的时间戳保存，界面按本地时区显示。
- 数据库与图片使用 Windows 当前用户 DPAPI 加密；运行中数据库在内存处理，成功写入原子替换的加密快照后才返回成功。
- API Key 以 DPAPI 密文写入 `settings.json`，界面不会收到原始密钥。留空保留已保存密钥，清除需要显式勾选。
- 服务地址只允许 HTTPS，或本机回环 HTTP；切换服务来源时需要重新填写或清除已保存密钥。HTTP 请求不跟随重定向。
- 配置损坏、密文校验失败时保留原文件并报告错误，不以默认值覆盖已有数据。
- AI 解析会把当前记录的文字/图片发送给设置中选择的服务。关闭“自动解析”后只在手动操作时发送。

## 数据与升级

数据目录为 `%APPDATA%/com.local.taskvault/`。旧版 `inbox.db` / `tasks.db` 会在首次启动新版时迁入统一的加密数据文件；旧数据的归档也使用加密形式。旧附件保留引用名称并原子升级为密文。迁移失败会中止启动，保留可恢复的数据。

加密数据依赖原 Windows 账户的 DPAPI 密钥；不要把加密文件复制到另一账户后就删除原环境。旧版应用不能直接读取新版加密数据。升级前应退出旧版 TaskVault。

## AI 服务

支持 OpenAI Chat Completions 兼容协议和 Anthropic Messages 协议。服务商预设只负责填入地址与模型名称，模型能力仍取决于服务商。

主模型和视觉模型可以分别指定，但共用协议、地址和密钥。OpenAI 兼容的本机回环服务可不填写密钥。环境变量 `DEEPSEEK_API_KEY` / `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` 分别只用于对应官方服务地址；自定义网关需在设置中明确填写密钥，不会继承全局环境密钥。

## 开发与验证

```powershell
cd src-tauri
cargo run
cargo test --offline --locked
cargo clippy --offline --locked --all-targets -- -D warnings
cargo build --release --offline --locked
```

如果正运行 `target/release/task-vault.exe`，请先从托盘退出，再覆盖构建。2026-09-16 的第二版界面交付另存为 `src-tauri/target/release/task-vault-redesign-v2.exe`；退出旧实例后再打开新版，否则单实例机制只会唤起旧界面。

前端独立预览（仅合成数据，不连接真实数据库或 AI）：

```powershell
node scripts/preview.cjs
```

按终端打印的回环地址访问。该服务器只允许访问明确列出的 UI 资源；操作只影响页面内存，刷新即重置。

前端回归：`node ui/tests/time-format.test.js` 和 `node ui/tests/task-views.test.js`。浏览器预览不具备原生窗口控制能力，标题栏按钮会禁用；实际窗口行为由 Tauri 提供。

## 界面参考

第二版采用石墨灰背景、蓝紫强调色和细分隔任务行。只读调研了 [Planify](https://github.com/alainm23/planify) 和 [AppFlowy](https://github.com/AppFlowy-IO/AppFlowy)，实际视觉参考为 Planify README 的[官方深色截图](https://raw.githubusercontent.com/alainm23/planify/main/data/resources/screenshot/screenshot-03.png)：借鉴任务分组和紧凑列表的信息层次，保留本项目的记录确认流程；未复制其源码。AppFlowy README 的历史任务截图链接已失效，未将其作为视觉核验依据。

本地静态图标来自 Lucide `0.468.0`，来源及 ISC 许可见 `ui/icons/README.md`、`ui/icons/LICENSE`，运行时无需联网加载图标。

## 源码导航

| 路径 | 职责 |
|---|---|
| `src-tauri/src/lib.rs` | 生命周期、窗口、托盘、共享状态与启动恢复 |
| `src-tauri/src/commands.rs` | UI 命令边界 |
| `src-tauri/src/db.rs` | 数据迁移、事务、加密快照、请求代次与转正幂等 |
| `src-tauri/src/ai_queue.rs` | 有界 AI 执行、失败持久化、恢复 |
| `src-tauri/src/deepseek.rs` | 两种 AI 协议、提示词、结果容错 |
| `src-tauri/src/storage.rs` | Windows DPAPI 与原子文件替换 |
| `src-tauri/src/attachments.rs` | 图片输入校验、加密附件与旧文件迁移 |
| `src-tauri/src/settings.rs` | 密钥保护、脱敏配置与地址验证 |
| `ui/` | 深色工作台、详情、设置与快速录入 |

自动排期、到期提醒、时间线、跨账户导出暂未实现。

## 分支工作流

`dev` 是开发主线，`main` 是稳定发布分支。合入 `main` 与发布需要人工审批。本轮修复位于 `codex/taskvault-reliability-ui`。
