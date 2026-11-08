# ai-qa-report

扫描本机各 AI 编程助手/聊天工具的会话记录，取每个会话的首条用户消息，过滤测试与重复会话后生成一份可筛选、可搜索的暗色 HTML 报告（时间倒序、按工具分类统计）。

单文件二进制，无运行时依赖；同一份源码可构建 Windows / Linux / macOS。

当前版本：**v0.9.2**（2026-11-09）

- 报告分页（10 / 20 / 30 / 50 / 100 / 200 条每页，默认 50），搜索与工具筛选后自动重新分页
- 每行 `📂` 打开会话所在文件夹（Windows 资源管理器选中文件 / macOS Finder / Linux xdg-open）
- 单条 / 批量删除，回收站（`/recycle`）逐条恢复，清空时移入系统回收站/废纸篓
- 回收站每行可单独"删除"（单条移入系统回收站，可恢复）
- Windows x64 / x86，Linux x64 / arm64，macOS Intel / Apple Silicon
- Electron 桌面端（Windows portable；macOS dmg/zip 由 CI 构建）

## 平台支持

| 平台 | 架构 | 产物 | 删除 / 恢复 | 清空到系统回收站 |
|---|---|---|---|---|
| Windows | x64 | `ai-qa-report-windows-x64.exe` | ✅ | ✅ 系统回收站 |
| Windows | x86（32 位） | `ai-qa-report-windows-x86.exe` | ✅ | ✅ 系统回收站 |
| Linux | x64 | `ai-qa-report-linux-x64` | ✅ | ✅ `gio trash`（回退 `trash-put`） |
| Linux | arm64 | `ai-qa-report-linux-arm64` | ✅ | ✅ 同上 |
| macOS | Intel x64 | `ai-qa-report-macos-x64` | ✅ | ✅ 移入 `~/.Trash` |
| macOS | Apple Silicon arm64 | `ai-qa-report-macos-arm64` | ✅ | ✅ 移入 `~/.Trash` |

> Windows x86 与 macOS / Linux 包由 GitHub Actions 构建；本机默认只构建当前平台。
> macOS / Linux 的"清空"路径按各平台标准实现，首次使用建议先用测试目录验证。

## 覆盖的数据源

| 工具 | 记录位置 | 平台 |
|---|---|---|
| ZCode | `~/.zcode/cli/db/db.sqlite` | 全平台（目录存在才扫描） |
| Codex CLI | `~/.codex/sessions/**/*.jsonl(.zst)`、`~/.codex/archived_sessions/` | 全平台 |
| Claude Code | `~/.claude/projects/**/*.jsonl` | 全平台 |
| Gemini Antigravity | `~/.gemini/antigravity/conversations/*.pb`（加密，仅列元数据） | 全平台（目录存在才扫描） |
| 豆包桌面版 | `~/Doubao/chats/<日期>/<会话>/` | Windows（其他平台无此目录则跳过） |
| WorkBuddy / WorkBuddy AI | `~/WorkBuddy/`、`~/WorkBuddy AI/` | Windows |
| LobsterAI | `%APPDATA%/LobsterAI/lobsterai.sqlite` | Windows |

不存在的数据源会被自动跳过，所以在任何一个平台上都能跑，只是列出的内容不同。

## 使用

```bash
# 报告生成在 exe 所在目录：AI问答记录梳理报告.html
./ai-qa-report

# 自定义输出位置
AI_QA_OUT=/tmp ./ai-qa-report
```

### 删除与恢复（本地服务模式）

```bash
# 启动本地服务（默认 8765，可指定端口）
./ai-qa-report serve
./ai-qa-report serve 8899
```

浏览器打开 `http://127.0.0.1:8765`：

- 列表默认每页 50 条，可切 10 / 20 / 30 / 50 / 100 / 200，顶部"上一页 / 下一页"翻页；搜索和工具筛选都会重新分页
- 每行 `📂` = 打开该会话所在文件夹（Windows 资源管理器选中文件 / macOS Finder `open -R` / Linux `xdg-open`）
- 行首勾选后可 **批量删除选中**；也可点行尾 `✕` 单条删除
- 删除不是永久删除：文件/目录型会话移入 `recycle/<工具>-<毫秒>-<序号>-<原名>/`；数据库型（ZCode/LobsterAI）先 `VACUUM INTO` 整库快照再删行
- 每次删除写 `manifest.json`，右上角"🗑 回收站"（`/recycle`）可一键恢复
- 恢复：文件型移回原位置（不覆盖已存在的同名文件）；数据库型从快照把会话及子表记录回插

命令行恢复：

```bash
./ai-qa-report restore <回收条目名>
```

### 清空回收站

`/recycle` 页点"清空回收站"，输入 `清空` 确认。行为按平台区分：

- Windows：移入系统回收站（PowerShell `SendToRecycleBin`）
- macOS：移入 `~/.Trash`
- Linux：`gio trash`，不可用时回退 `trash-put`

清空不是不可逆删除；需要彻底释放空间时，再清空系统回收站/废纸篓。

回收站每行右侧除"↩ 恢复"外还有"🗑 删除"：点一次变为"确认删除?"，再点一次把该条目移入系统回收站/废纸篓，不影响其他条目。

## 桌面端（Electron）

`desktop/` 是 Electron 桌面壳：启动时自动拉起内置 Rust 内核（随机本地端口），窗口里就是完整报告与删除/回收站界面；退出应用会自动结束后台内核。

报告与回收站数据放在应用数据目录，例如 Windows 为 `%APPDATA%\ai-qa-report-desktop\data`。

```bash
# Windows x64 portable
cd desktop
npm ci
npm run dist:win
# 产物: desktop/dist/AI-QA-Report-<version>-win-x64.exe
```

macOS 需要在 macOS 上先构建对应架构的内核，再打包：

```bash
cargo build --release --target aarch64-apple-darwin   # 或 x86_64-apple-darwin
mkdir -p desktop/bin/mac
cp target/aarch64-apple-darwin/release/ai-qa-report desktop/bin/mac/
cd desktop && npm ci && npm run dist:mac
```

也可以推 `desktop-v*` tag 或手动触发 `.github/workflows/desktop-build.yml`，由 CI 产出 Windows portable 和 macOS arm64/x64 的 dmg/zip（未签名，首次打开需右键"打开"）。

## 项目结构

```text
ai-qa-report-rs/
├─ src/main.rs                          扫描 / 过滤 / 删除 / 回收站 / 清空 / 本地 HTTP 服务
├─ src/template.html                    报告页（分页、筛选、批量删除、📂）
├─ desktop/main.js                      Electron 主进程（拉起内核、开窗、退出清理）
├─ desktop/package.json                 electron-builder 配置
├─ desktop/bin/{win,mac}/               内置内核（Windows 已带，macOS 由 CI 构建）
├─ .github/workflows/build.yml          CLI 六平台构建
└─ .github/workflows/desktop-build.yml  桌面端 Windows / macOS 构建
```

## 更新记录

| 版本 | 主要内容 |
|---|---|
| v0.9.2 | 回收站每行可单独删除（移入系统回收站） |
| v0.9.1 | 分页档位 10 / 20 / 30 / 50 / 100 / 200（默认 50） |
| v0.9.0 | 报告分页；每行 `📂` 打开所在文件夹 |
| v0.8.0 | 多端：Windows x86、macOS `~/.Trash`、Linux `gio trash` / `trash-put` |
| v0.7.x | 回收站"清空"（移入系统回收站）；修复内嵌浏览器拦截 `prompt` |
| v0.6.x | 批量删除、回收站页面、一键恢复、`manifest.json` |
| v0.5 | ZCode / LobsterAI 数据库会话删除（整库快照） |

## 从源码构建

```bash
cargo build --release
# 产物: target/release/ai-qa-report(.exe)
```

按目标构建：

```bash
# Windows x64 / x86（在 Windows 上）
rustup target add x86_64-pc-windows-msvc i686-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
cargo build --release --target i686-pc-windows-msvc

# Linux x64
cargo build --release --target x86_64-unknown-linux-gnu

# macOS（在 macOS 上）
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
```

## 下载预编译版本

推送到 GitHub 后，Actions 会为 6 个目标自动构建：

`ai-qa-report-windows-x64.exe` / `ai-qa-report-windows-x86.exe` / `ai-qa-report-linux-x64` / `ai-qa-report-linux-arm64` / `ai-qa-report-macos-arm64` / `ai-qa-report-macos-x64`

打 `v*` tag 时会自动附带发布到 Releases。

## 过滤规则

- 剔除测试会话：`hi` / `hello` / `你好` / `你是什么模型` 等（见 `src/main.rs` 的 `TRIVIAL`）
- 剔除同一工具内首条消息完全相同的重复会话
- 剔除系统自动注入的 agent history 任务

## License

MIT
