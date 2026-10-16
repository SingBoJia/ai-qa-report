# ai-qa-report

扫描本机各 AI 编程助手/聊天工具的会话记录，取每个会话的首条用户消息，过滤测试与重复会话后生成一份可筛选、可搜索的暗色 HTML 报告（时间倒序、按工具分类统计）。

单文件二进制，无运行时依赖，支持 Windows / Linux / macOS。

## 覆盖的数据源

| 工具 | 记录位置 |
|---|---|
| ZCode | `~/.zcode/cli/db/db.sqlite` |
| Codex CLI | `~/.codex/sessions/**/*.jsonl(.zst)`、`~/.codex/archived_sessions/` |
| Claude Code | `~/.claude/projects/**/*.jsonl` |
| Gemini Antigravity | `~/.gemini/antigravity/conversations/*.pb`（加密，仅列元数据） |
| 豆包桌面版 | `~/Doubao/chats/<日期>/<会话>/`（落盘产物） |
| WorkBuddy / WorkBuddy AI | `~/WorkBuddy/`、`~/WorkBuddy AI/` |
| LobsterAI | `AppData/Roaming/LobsterAI/lobsterai.sqlite`（仅 Windows） |

不存在的数据源会被自动跳过，所以在任何一个平台上都能跑，只是列出的内容不同。

## 使用

```bash
# 报告生成在 exe 所在目录：AI问答记录梳理报告.html
./ai-qa-report

# 自定义输出位置
AI_QA_OUT=/tmp ./ai-qa-report
```

## 从源码构建

```bash
cargo build --release
# 产物: target/release/ai-qa-report(.exe)
```

## 下载预编译版本

推送到 GitHub 后，Actions 会为四个目标自动构建：
`ai-qa-report-windows-x64.exe` / `ai-qa-report-linux-x64` / `ai-qa-report-macos-arm64` (Apple Silicon) / `ai-qa-report-macos-x64` (Intel Mac)。

打 `v*` tag 时会自动附带发布到 Releases。

## 过滤规则

- 剔除测试会话：`hi` / `hello` / `你好` / `你是什么模型` 等（见 `src/main.rs` 的 `TRIVIAL`）
- 剔除同一工具内首条消息完全相同的重复会话
- 剔除系统自动注入的 agent history 任务

## License

MIT
