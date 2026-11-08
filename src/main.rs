use anyhow::Result;
use chrono::{DateTime, Local, TimeZone};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn home_dir() -> PathBuf {
    // Windows: %USERPROFILE%; Unix: $HOME; fallback to cargo-provided dir
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(dirs_fallback()))
}

fn dirs_fallback() -> String {
    #[cfg(target_os = "windows")]
    return "C:/Users/Public".into();
    #[cfg(not(target_os = "windows"))]
    return "/tmp".into();
}

// Report output dir: next to the exe (portable), or AI_QA_OUT override
fn out_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("AI_QA_OUT") {
        return PathBuf::from(d);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

#[derive(Clone)]
struct Row {
    tool: &'static str,
    time: String,
    what: String,
    folder: String,
    detail: String,
    // 删除目标：tool|具体路径或id。空 = 不可删
    del_id: String,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn summ(text: &str) -> String {
    let t: String = text.lines().collect::<Vec<_>>().join(" ").trim().to_string();
    if t.is_empty() {
        return "(无可读文本/空会话)".into();
    }
    let chars: Vec<char> = t.chars().collect();
    if chars.len() > 160 {
        format!("{}…", chars[..160].iter().collect::<String>())
    } else {
        t
    }
}

fn ms2str(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => ms.to_string(),
    }
}

fn iso2str(s: &str) -> String {
    // "2026-07-03T15:22:33.444Z" -> "2026-07-03 15:22" (local time)
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        let local: DateTime<Local> = dt.into();
        local.format("%Y-%m-%d %H:%M").to_string()
    } else {
        s.get(..16).unwrap_or(s).replace('T', " ")
    }
}

// ---------- ZCode: ~/.zcode/cli/db/db.sqlite ----------
fn load_zcode() -> Result<Vec<Row>> {
    let db = format!("{}/.zcode/cli/db/db.sqlite", home_dir().display());
    if !Path::new(&db).exists() {
        return Ok(vec![]);
    }
    let con = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    con.busy_timeout(std::time::Duration::from_secs(3))?;
    let mut stmt = con.prepare(
        "SELECT title, directory, time_created, id FROM session ORDER BY time_created",
    )?;
    let mut rows = vec![];
    let iter = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    for r in iter {
        let (title, dir, ts, sid) = r?;
        rows.push(Row {
            tool: "ZCode",
            time: ts.map(ms2str).unwrap_or_default(),
            what: summ(&title.unwrap_or_default()),
            folder: "~/.zcode/cli/db/db.sqlite".into(),
            detail: format!("工作目录: {}", dir.unwrap_or_else(|| "-".into())),
            del_id: sid.map(|s| format!("db:zcode:{}", s)).unwrap_or_default(),
        });
    }
    Ok(rows)
}

// ---------- Claude Code: ~/.claude/projects/**/*.jsonl ----------
fn load_claude() -> Result<Vec<Row>> {
    let base = PathBuf::from(format!("{}/.claude/projects", home_dir().display()));
    let mut rows = vec![];
    if !base.exists() {
        return Ok(rows);
    }
    let mut projects: Vec<PathBuf> = fs::read_dir(&base)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    projects.sort();
    for pdir in projects {
        let proj = pdir.file_name().unwrap().to_string_lossy().to_string();
        let mut files: Vec<PathBuf> = walk_jsonl(&pdir);
        files.sort();
        for f in files {
            let (first_user, n_msg, t_first) = scan_claude_file(&f)?;
            if first_user.is_empty() && n_msg == 0 {
                continue;
            }
            let rel = f.to_string_lossy().replace('\\', "/");
            let rel_short = rel.split("/.claude/").nth(1).unwrap_or(&rel).to_string();
            rows.push(Row {
                tool: "Claude Code",
                time: t_first.map(|s| iso2str(&s)).unwrap_or_default(),
                what: summ(&first_user),
                folder: format!("~/.claude/{}", rel_short),
                detail: format!("项目: {} ｜ 用户消息数: {}", proj, n_msg),
                del_id: f.to_string_lossy().to_string(),
            });
        }
    }
    Ok(rows)
}

fn walk_jsonl(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk_jsonl(&p));
            } else if p.extension().map(|x| x == "jsonl").unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out
}

fn scan_claude_file(f: &Path) -> Result<(String, i32, Option<String>)> {
    let content = fs::read_to_string(f).unwrap_or_default();
    let mut first_user = String::new();
    let mut n_msg = 0i32;
    let mut t_first: Option<String> = None;
    for line in content.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
            if t_first.is_none() {
                t_first = Some(ts.to_string());
            }
        }
        if v.get("type").and_then(|x| x.as_str()) != Some("user") {
            continue;
        }
        if v.get("isMeta").and_then(|x| x.as_bool()).unwrap_or(false) {
            continue;
        }
        let msg = v.get("message").cloned().unwrap_or(Value::Null);
        let texts: Vec<String> = match msg.get("content") {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|c| {
                    if c.get("type").and_then(|x| x.as_str()) == Some("text") {
                        c.get("text").and_then(|x| x.as_str()).map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect(),
            _ => vec![],
        };
        for t in texts {
            let tt = t.trim();
            if tt.is_empty() || tt.starts_with('<') {
                continue;
            }
            if first_user.is_empty() {
                first_user = tt.to_string();
            }
            n_msg += 1;
        }
    }
    Ok((first_user, n_msg, t_first))
}

// ---------- Codex: ~/.codex/sessions/**/*.jsonl(.zst) + archived ----------
fn load_codex() -> Result<Vec<Row>> {
    let mut files: Vec<PathBuf> = vec![];
    for sub in ["sessions", "archived_sessions"] {
        let base = PathBuf::from(format!("{}/.codex/{}", home_dir().display(), sub));
        if !base.exists() {
            continue;
        }
        files.extend(walk_files(&base));
    }
    files.sort();
    let mut rows = vec![];
    for f in files {
        let (first_user, cwd, t_first) = scan_codex_file(&f)?;
        if first_user.is_empty() {
            continue;
        }
        let rel = f.to_string_lossy().replace('\\', "/");
        let rel_short = rel.split("/.codex/").nth(1).unwrap_or(&rel).to_string();
        let rel_short = rel_short.chars().take(80).collect::<String>();
        rows.push(Row {
            tool: "Codex",
            time: t_first.map(|s| iso2str(&s)).unwrap_or_default(),
            what: summ(&first_user),
            folder: format!("~/.codex/{}", rel_short),
            detail: if cwd.is_empty() {
                String::new()
            } else {
                format!("工作目录: {}", cwd)
            },
            del_id: f.to_string_lossy().to_string(),
        });
    }
    Ok(rows)
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk_files(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn scan_codex_file(f: &Path) -> Result<(String, String, Option<String>)> {
    let raw = fs::read(f)?;
    let text = if f.extension().map(|x| x == "zst").unwrap_or(false) {
        let mut dec = zstd::stream::Decoder::new(&raw[..])?;
        let mut buf = String::new();
        dec.read_to_string(&mut buf)?;
        buf
    } else {
        String::from_utf8_lossy(&raw).to_string()
    };
    let mut first_user = String::new();
    let mut cwd = String::new();
    let mut t_first: Option<String> = None;
    for line in text.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
            if t_first.is_none() {
                t_first = Some(ts.to_string());
            }
        }
        if cwd.is_empty() {
            if let Some(c) = v.get("cwd").and_then(|x| x.as_str()) {
                cwd = c.to_string();
            } else if let Some(p) = v.get("payload") {
                if let Some(c) = p.get("cwd").and_then(|x| x.as_str()) {
                    cwd = c.to_string();
                }
            }
        }
        let payload = v.get("payload").cloned().unwrap_or(Value::Null);
        let pt = payload.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let is_user = pt == "user_message"
            || (pt == "message" && payload.get("role").and_then(|x| x.as_str()) == Some("user"));
        if !is_user {
            continue;
        }
        let txt: String = if let Some(m) = payload.get("message").and_then(|x| x.as_str()) {
            m.to_string()
        } else if let Some(Value::Array(arr)) = payload.get("content") {
            arr.iter()
                .filter_map(|c| c.get("text").and_then(|x| x.as_str()).map(|s| s.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        } else if let Some(s) = payload.get("content").and_then(|x| x.as_str()) {
            s.to_string()
        } else {
            String::new()
        };
        let t = txt.trim();
        if t.is_empty() || t.starts_with('<') || t.starts_with('#') {
            continue;
        }
        if first_user.is_empty() {
            first_user = t.to_string();
        }
    }
    Ok((first_user, cwd, t_first))
}

// ---------- 豆包: ~/Doubao/chats/<date>/<chat>/ ----------
fn load_doubao() -> Result<Vec<Row>> {
    let base = PathBuf::from(format!("{}/Doubao/chats", home_dir().display()));
    let mut rows = vec![];
    if !base.exists() {
        return Ok(rows);
    }
    let mut dates: Vec<PathBuf> = fs::read_dir(&base)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dates.sort();
    for dpath in dates {
        let date = dpath.file_name().unwrap().to_string_lossy().to_string();
        let mut chats: Vec<PathBuf> = fs::read_dir(&dpath)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        chats.sort();
        for cpath in chats {
            let chat = cpath.file_name().unwrap().to_string_lossy().to_string();
            let mut files: Vec<String> = vec![];
            collect_rel_files(&cpath, &cpath, &mut files);
            files.sort();
            let n_files = files.len();
            // hint: first line of first .md file
            let mut hint = String::new();
            for fn_ in &files {
                if fn_.ends_with(".md") {
                    let p = cpath.join(fn_.replace('/', "\\"));
                    if let Ok(txt) = fs::read_to_string(&p) {
                        if let Some(l) = txt.lines().next() {
                            if !l.trim().is_empty() {
                                hint = l.trim().chars().take(150).collect();
                                break;
                            }
                        }
                    }
                }
            }
            if hint.is_empty() {
                hint = files.first().cloned().unwrap_or_default();
            }
            let what = if hint.is_empty() {
                "(仅对话，本地无落盘产物)".to_string()
            } else {
                hint
            };
            rows.push(Row {
                tool: "豆包",
                time: date.clone(),
                what: summ(&what),
                folder: format!("~/Doubao/chats/{}/{}", date, chat),
                detail: format!(
                    "落盘文件数: {} ｜ {}",
                    n_files,
                    files.iter().take(3).cloned().collect::<Vec<_>>().join("、")
                ),
                del_id: cpath.to_string_lossy().to_string(),
            });
        }
    }
    Ok(rows)
}

fn collect_rel_files(base: &Path, dir: &Path, out: &mut Vec<String>) {
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                collect_rel_files(base, &p, out);
            } else {
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(rel);
            }
        }
    }
}

// ---------- Gemini Antigravity: ~/.gemini/antigravity/conversations/*.pb ----------
// 会话体 .pb 是 ChaCha20-Poly1305 加密，但标题索引在 ~/.gemini/antigravity/agyhub_summaries_proto.pb
// （未加密 protobuf：record{1:uuid, 2{1:title, ...}, ...}），解析它拿 uuid->title。
fn parse_pb_fields(buf: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut out = vec![];
    let mut i = 0usize;
    let read_varint = |i: &mut usize| -> Option<u64> {
        let mut r: u64 = 0;
        let mut s: u32 = 0;
        loop {
            let b = *buf.get(*i)?; *i += 1;
            r |= ((b & 0x7f) as u64) << s;
            if b & 0x80 == 0 { return Some(r); }
            s += 7;
            if s > 63 { return None; }
        }
    };
    while i < buf.len() {
        let key = match read_varint(&mut i) { Some(k) => k, None => break };
        let (fnum, ftype) = ((key >> 3) as u32, key & 7);
        match ftype {
            0 => { if read_varint(&mut i).is_none() { break } out.push((fnum, vec![])); }
            2 => {
                let ln = match read_varint(&mut i) { Some(l) => l as usize, None => break };
                if i + ln > buf.len() { break }
                out.push((fnum, buf[i..i + ln].to_vec()));
                i += ln;
            }
            5 => { if i + 4 > buf.len() { break } i += 4; }
            1 => { if i + 8 > buf.len() { break } i += 8; }
            _ => break,
        }
    }
    out
}

fn load_ag_titles() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let p = home_dir().join(".gemini/antigravity/agyhub_summaries_proto.pb");
    let Ok(raw) = fs::read(&p) else { return map };
    for (_, rec) in parse_pb_fields(&raw) {
        let mut uuid = String::new();
        let mut title = String::new();
        for (f2, v2) in parse_pb_fields(&rec) {
            if f2 == 1 && uuid.is_empty() {
                if let Ok(s) = String::from_utf8(v2.clone()) {
                    if s.len() == 36 && s.as_bytes()[8] == b'-' { uuid = s; }
                }
            }
            if f2 == 2 && title.is_empty() {
                for (f3, v3) in parse_pb_fields(&v2) {
                    if f3 == 1 {
                        if let Ok(s) = String::from_utf8(v3) { title = s; }
                        break;
                    }
                }
            }
            if !uuid.is_empty() && !title.is_empty() { break }
        }
        if !uuid.is_empty() && !title.is_empty() {
            map.insert(uuid, title);
        }
    }
    map
}

fn load_antigravity() -> Result<Vec<Row>> {
    let base = home_dir().join(".gemini/antigravity/conversations");
    let titles = load_ag_titles();
    let mut rows = vec![];
    if !base.exists() {
        return Ok(rows);
    }
    let mut files: Vec<PathBuf> = fs::read_dir(&base)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "pb").unwrap_or(false))
        .collect();
    files.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
    for f in files {
        let meta = fs::metadata(&f)?;
        let mt: DateTime<Local> = meta.modified().map(|m| m.into())?;
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let what = match titles.get(&stem) {
            Some(t) if !t.trim().is_empty() => t.clone(),
            _ => "(标题不在摘要索引中，会话体加密)".into(),
        };
        rows.push(Row {
            tool: "Gemini Antigravity",
            time: mt.format("%Y-%m-%d %H:%M").to_string(),
            what: summ(&what),
            folder: format!("~/.gemini/antigravity/conversations/{}.pb", stem),
            detail: format!("大小: {} KB", meta.len() / 1024),
            del_id: f.to_string_lossy().to_string(),
        });
    }
    Ok(rows)
}

// ---------- WorkBuddy: ~/WorkBuddy & ~/WorkBuddy AI ----------
fn load_workbuddy() -> Result<Vec<Row>> {
    let mut rows = vec![];
    for (sub, tool) in [("WorkBuddy", "WorkBuddy"), ("WorkBuddy AI", "WorkBuddy AI")] {
        let base = PathBuf::from(format!("{}/{}", home_dir().display(), sub));
        if !base.exists() {
            continue;
        }
        let mut dirs: Vec<PathBuf> = fs::read_dir(&base)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir() && p.file_name().map(|n| n != "Claw").unwrap_or(false))
            .collect();
        dirs.sort();
        for d in dirs {
            let time_raw = d.file_name().unwrap().to_string_lossy().to_string();
            let mut names: Vec<String> = fs::read_dir(&d)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            let real: Vec<String> = names.iter().filter(|n| !n.starts_with('.')).cloned().collect();
            let what = if !real.is_empty() {
                format!("产物: {}", real.iter().take(4).cloned().collect::<Vec<_>>().join("、"))
            } else {
                "(空会话/仅状态文件)".to_string()
            };
            rows.push(Row {
                tool,
                time: time_raw.chars().take(16).collect::<String>().replace('_', " "),
                what: summ(&what),
                folder: {
                    let hd = home_dir().to_string_lossy().replace('\\', "/");
                    d.to_string_lossy().replace('\\', "/").replace(&hd, "~")
                },
                detail: real.iter().take(6).cloned().collect::<Vec<_>>().join("、"),
                del_id: d.to_string_lossy().to_string(),
            });
        }
    }
    Ok(rows)
}

// ---------- LobsterAI: AppData sqlite ----------
fn load_lobster() -> Result<Vec<Row>> {
    let db = format!("{}/AppData/Roaming/LobsterAI/lobsterai.sqlite", home_dir().display());
    if !Path::new(&db).exists() {
        return Ok(vec![]);
    }
    let con = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    con.busy_timeout(std::time::Duration::from_secs(3))?;
    let mut stmt = con.prepare(
        "SELECT title, status, cwd, created_at, id FROM cowork_sessions ORDER BY created_at",
    )?;
    let mut rows = vec![];
    let iter = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    for r in iter {
        let (title, status, cwd, ts, id) = r?;
        rows.push(Row {
            tool: "LobsterAI",
            time: ms2str(ts.unwrap_or(0)),
            what: summ(&title.unwrap_or_default()),
            folder: "~/AppData/Roaming/LobsterAI/lobsterai.sqlite".into(),
            detail: format!(
                "状态: {} ｜ 工作目录: {}",
                status.unwrap_or_else(|| "-".into()),
                cwd.unwrap_or_else(|| "-".into())
            ),
            del_id: id.map(|i| format!("db:lobster:{}", i)).unwrap_or_default(),
        });
    }
    Ok(rows)
}

// ---------- 过滤：测试会话 / 系统自动任务 / 重复 ----------
fn normkey(t: &str) -> String {
    let mut out = String::new();
    for c in t.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(&c) {
            out.push(c);
        }
    }
    out
}

const TRIVIAL: &[&str] = &[
    "hi", "hello", "你好", "哈喽", "嗨", "halo", "test", "测试", "在吗", "ping",
    "helloworld", "你是谁", "你是什么模型", "现在是几点", "几点了", "用中文回复我",
    "你可以写代码吗",
];

struct Filtered {
    rows: Vec<Row>,
    n_trivial: usize,
    n_auto: usize,
    n_dupe: usize,
}

fn filter_rows(all: Vec<Row>) -> Filtered {
    let mut kept = vec![];
    let mut seen = std::collections::HashSet::new();
    let (mut n_trivial, mut n_auto, mut n_dupe) = (0usize, 0usize, 0usize);
    for r in all {
        let k = normkey(&r.what);
        if TRIVIAL.contains(&k.as_str()) {
            n_trivial += 1;
            continue;
        }
        if k.starts_with("thefollowingisthecodexagenthistory") {
            n_auto += 1;
            continue;
        }
        if r.what.starts_with('(') {
            kept.push(r); // 占位行（加密/空会话）不去重
            continue;
        }
        let dk = (r.tool, k);
        if seen.contains(&dk) {
            n_dupe += 1;
            continue;
        }
        seen.insert(dk);
        kept.push(r);
    }
    Filtered { rows: kept, n_trivial, n_auto, n_dupe }
}

const TOOL_COLOR: &[(&str, &str)] = &[
    ("ZCode", "#7c5cff"),
    ("Codex", "#10a37f"),
    ("Claude Code", "#d97757"),
    ("豆包", "#3b82f6"),
    ("Gemini Antigravity", "#8b5cf6"),
    ("WorkBuddy", "#0ea5e9"),
    ("WorkBuddy AI", "#0ea5e9"),
    ("LobsterAI", "#e11d48"),
];

fn build_html(f: &Filtered) -> String {
    let mut counts: Vec<(&str, usize)> = vec![];
    for r in &f.rows {
        if let Some(c) = counts.iter_mut().find(|(t, _)| *t == r.tool) {
            c.1 += 1;
        } else {
            counts.push((r.tool, 1));
        }
    }
    let total = f.rows.len();

    let mut trs = String::new();
    for (idx, r) in f.rows.iter().enumerate() {
        let color = TOOL_COLOR
            .iter()
            .find(|(t, _)| *t == r.tool)
            .map(|(_, c)| *c)
            .unwrap_or("#64748b");
        let del_btn = if r.del_id.is_empty() {
            String::new()
        } else {
            format!(
                "<button class=\"del\" onclick=\"delRow({},this)\" title=\"删除（移入回收目录/整库备份，可恢复）\">✕</button>",
                idx
            )
        };
        let sel_box = if r.del_id.is_empty() {
            String::new()
        } else {
            "<input type=\"checkbox\" class=\"sel\" onclick=\"syncSel()\">".to_string()
        };
        let open_btn = if r.del_id.is_empty() {
            String::new()
        } else {
            format!(
                "<button class=\"fo\" onclick=\"openFolder({},this)\" title=\"打开所在文件夹\">📂</button>",
                idx
            )
        };
        // 行内容校验键：tool+del_id 整行，供 serve 端与 delmap.tsv 比对，防止重建后行号错位删错
        let rowkey = esc(&format!("{}\t{}", r.tool, r.del_id));
        trs.push_str(&format!(
            "<tr data-tool=\"{}\" data-k=\"{}\"><td class=\"chk\">{}</td><td class=\"t\">{}</td><td><span class=\"badge\" style=\"background:{}\">{}</span></td><td class=\"w\">{}<div class=\"d\">{}</div></td><td class=\"f\">{}</td><td class=\"delc\">{}</td></tr>\n",
            esc(r.tool), rowkey, sel_box, esc(r.time.get(..16).unwrap_or(&r.time)), color, esc(r.tool),
            esc(&r.what), esc(&r.detail), esc(&r.folder), format!("{}{}", open_btn, del_btn)
        ));
    }

    let mut chips = String::from("<button class=\"chip active\" onclick=\"filt('',this)\">全部</button>");
    let mut cards = String::new();
    for (t, c) in TOOL_COLOR {
        if let Some((_, n)) = counts.iter().find(|(x, _)| x == t) {
            chips.push_str(&format!(
                "<button class=\"chip\" onclick=\"filt('{}',this)\" style=\"border-color:{}\">{} <b>{}</b></button>",
                esc(t), c, esc(t), n
            ));
            cards.push_str(&format!(
                "<div class=\"card\"><div class=\"n\" style=\"color:{}\">{}</div><div class=\"l\">{}</div></div>",
                c, n, esc(t)
            ));
        }
    }

    let template = include_str!("template.html");
    template
        .replace("__CARDS__", &cards)
        .replace("__CHIPS__", &chips)
        .replace("__ROWS__", &trs)
        .replace("__TOTAL__", &total.to_string())
        .replace("__TRIVIAL__", &f.n_trivial.to_string())
        .replace("__DUPE__", &f.n_dupe.to_string())
        .replace("__AUTO__", &f.n_auto.to_string())
        .replace("__REMOVED__", &(f.n_trivial + f.n_dupe + f.n_auto).to_string())
        .replace("__GENTIME__", &Local::now().format("%Y-%m-%d").to_string())
        .replace("__VERSION__", VERSION)
}

fn main() -> Result<()> {
    // ai-qa-report.exe [--version|version] | serve [port] | (默认) 生成报告
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && (args[1] == "--version" || args[1] == "-V" || args[1] == "version") {
        println!("ai-qa-report v{} (rust)", VERSION);
        println!("v0.1 首版提取 | v0.2 过滤测试/重复会话 | v0.3 体积优化+计划任务 | v0.4 Antigravity标题解密索引+serve删除模式 | v0.5 ZCode/LobsterAI数据库会话可删除+整库备份+删除行号校验修复");
        println!("v0.6 批量删除+勾选模式 | 回收站页面 /recycle | 一键恢复（文件回原位 / 数据库按快照回插）| 每条删除写 manifest.json");
        println!("v0.6.1 删除时目标已被应用清理不再报失败，按“跳过”处理并自动刷新列表");
        println!("v0.6.2 回收目录按“毫秒+序号+原名”唯一命名，同一批删除不再合并到同一文件夹");
        println!("v0.7.0 回收站支持“清空”：移入 Windows 系统回收站（仍可恢复，不做不可逆删除）");
        println!("v0.7.1 清空改用页面内确认框，避免内嵌浏览器拦截 prompt 导致点击无反应");
        println!("v0.8.0 多端：Windows x86 构建 | macOS 清空移入 ~/.Trash | Linux 清空走 gio trash / trash-put");
        println!("v0.9.0 报告分页（50/100/200 可选）| 每行“打开所在文件夹”（资源管理器/Finder/xdg-open）");
        println!("v0.9.1 分页档位改为 10/20/30/50/100/200（默认 50）");
        println!("v0.9.2 回收站每行可单独删除（移入系统回收站，可恢复）");
        return Ok(());
    }
    if args.len() >= 2 && args[1] == "restore" {
        let name = args.get(2).ok_or_else(|| {
            anyhow::anyhow!("用法: ai-qa-report.exe restore <回收条目名>（条目名见 serve 模式 /recycle 页面）")
        })?;
        let restored = restore_entry(name)?;
        println!("已恢复: {}", restored);
        return Ok(());
    }
    if args.len() >= 2 && args[1] == "recycle" {
        println!("{}", recycle_root().display());
        return Ok(());
    }
    if args.len() >= 2 && args[1] == "serve" {
        let port: u16 = args.get(2).map(|p| p.parse().unwrap_or(8765)).unwrap_or(8765);
        return serve(port);
    }
    run_build()?;
    println!("提示: 需要页面内删除/恢复时，运行 ai-qa-report.exe serve，再打开 http://127.0.0.1:8765");
    Ok(())
}

fn run_build() -> Result<()> {
    println!("ai-qa-report v{}", VERSION);
    println!("[1/4] 扫描数据源…");
    let mut all = vec![];
    all.extend(load_zcode().unwrap_or_else(|e| { eprintln!("  zcode 失败: {e}"); vec![] }));
    all.extend(load_claude().unwrap_or_else(|e| { eprintln!("  claude 失败: {e}"); vec![] }));
    all.extend(load_codex().unwrap_or_else(|e| { eprintln!("  codex 失败: {e}"); vec![] }));
    all.extend(load_doubao().unwrap_or_else(|e| { eprintln!("  doubao 失败: {e}"); vec![] }));
    all.extend(load_antigravity().unwrap_or_else(|e| { eprintln!("  antigravity 失败: {e}"); vec![] }));
    all.extend(load_workbuddy().unwrap_or_else(|e| { eprintln!("  workbuddy 失败: {e}"); vec![] }));
    all.extend(load_lobster().unwrap_or_else(|e| { eprintln!("  lobster 失败: {e}"); vec![] }));
    println!("  原始会话: {} 条", all.len());

    println!("[2/4] 过滤测试/重复/自动会话…");
    all.sort_by(|a, b| b.time.cmp(&a.time));
    let f = filter_rows(all);
    println!("  剔除 测试{} / 重复{} / 自动{}，保留 {} 条",
        f.n_trivial, f.n_dupe, f.n_auto, f.rows.len());

    // 保存行索引 → 删除目标映射，供 serve 模式删除时定位
    let map: Vec<String> = f.rows.iter().map(|r| format!("{}\t{}", r.tool, r.del_id)).collect();
    let _ = fs::create_dir_all(out_dir());
    fs::write(out_dir().join("delmap.tsv"), map.join("\n"))?;

    println!("[3/4] 生成 HTML…");
    let html = build_html(&f);

    println!("[4/4] 写出报告…");
    let out = out_dir().join("AI问答记录梳理报告.html");
    let out = out.to_string_lossy().to_string();
    fs::write(&out, html)?;
    println!("完成: {}", out);
    Ok(())
}

// ---------- 删除：文件型移入回收目录；数据库型走 trash_db（均可恢复） ----------
fn trash_target(tool: &str, target: &str) -> Result<String> {
    if let Some(spec) = target.strip_prefix("db:") {
        return trash_db(tool, spec);
    }
    let p = PathBuf::from(target);
    if !p.exists() {
        // 扫描后、点击前被应用自己清掉的会话：按“已失效”处理，由调用方刷新列表，不算失败
        return Ok(String::new());
    }
    // 安全: 只允许删已知数据源前缀下的路径
    let hd = home_dir().to_string_lossy().replace('\\', "/");
    let norm = target.replace('\\', "/");
    let allowed = [
        format!("{}/.claude/projects/", hd),
        format!("{}/.codex/sessions/", hd),
        format!("{}/.codex/archived_sessions/", hd),
        format!("{}/Doubao/chats/", hd),
        format!("{}/.gemini/antigravity/conversations/", hd),
        format!("{}/WorkBuddy/", hd),
        format!("{}/WorkBuddy AI/", hd),
    ];
    if !allowed.iter().any(|a| norm.starts_with(a.as_str())) {
        anyhow::bail!("路径不在允许的数据源目录内，拒绝删除");
    }
    let dest_dir = out_dir()
        .join("recycle")
        .join(format!("{}-{}", tool.replace(' ', "_"), unique_suffix(&p)));
    fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(p.file_name().unwrap_or_default());
    let is_dir = p.is_dir();
    if is_dir {
        fs_extra_like_move_dir(&p, &dest)?;
    } else {
        fs::rename(&p, &dest).or_else(|_| {
            fs::copy(&p, &dest)?;
            fs::remove_file(&p)
        })?;
    }
    write_manifest(&dest_dir, tool, if is_dir { "dir" } else { "file" }, target, None, None)?;
    Ok(dest_dir.to_string_lossy().to_string())
}

// ---------- 打开所在文件夹：Explorer / Finder / xdg-open ----------
fn reveal_target(tool: &str, target: &str) -> Result<String> {
    if let Some(spec) = target.strip_prefix("db:") {
        let Some((kind, _id)) = spec.split_once(':') else {
            anyhow::bail!("无效的数据库目标: {}", spec);
        };
        let db = match kind {
            "zcode" => format!("{}/.zcode/cli/db/db.sqlite", home_dir().display()),
            "lobster" => format!(
                "{}/AppData/Roaming/LobsterAI/lobsterai.sqlite",
                home_dir().display()
            ),
            _ => anyhow::bail!("未知的数据库类型: {}", kind),
        };
        if !Path::new(&db).exists() {
            anyhow::bail!("数据库不存在: {}", db);
        }
        open_in_file_manager(Path::new(&db), false)?;
        return Ok(db);
    }
    let p = PathBuf::from(target);
    if !p.exists() {
        anyhow::bail!("目标不存在（可能已被删除或应用清理）");
    }
    // 与删除同样的白名单：只允许已知数据源目录
    let hd = home_dir().to_string_lossy().replace('\\', "/");
    let norm = target.replace('\\', "/");
    let allowed = [
        format!("{}/.claude/projects/", hd),
        format!("{}/.codex/sessions/", hd),
        format!("{}/.codex/archived_sessions/", hd),
        format!("{}/Doubao/chats/", hd),
        format!("{}/.gemini/antigravity/conversations/", hd),
        format!("{}/WorkBuddy/", hd),
        format!("{}/WorkBuddy AI/", hd),
    ];
    if !allowed.iter().any(|a| norm.starts_with(a.as_str())) {
        anyhow::bail!("路径不在允许的数据源目录内，拒绝打开");
    }
    let is_dir = p.is_dir();
    open_in_file_manager(&p, is_dir)?;
    let _ = tool;
    Ok(target.to_string())
}

#[cfg(target_os = "windows")]
fn open_in_file_manager(path: &Path, is_dir: bool) -> Result<()> {
    let mut cmd = std::process::Command::new("explorer.exe");
    if is_dir {
        cmd.arg(path);
    } else {
        cmd.arg("/select,").arg(path);
    }
    cmd.spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_in_file_manager(path: &Path, is_dir: bool) -> Result<()> {
    let mut cmd = std::process::Command::new("open");
    if !is_dir {
        cmd.arg("-R");
    }
    cmd.arg(path).spawn()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_in_file_manager(path: &Path, is_dir: bool) -> Result<()> {
    let target = if is_dir {
        path.to_path_buf()
    } else {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| path.to_path_buf())
    };
    std::process::Command::new("xdg-open").arg(target).spawn()?;
    Ok(())
}

// ---------- 数据库会话删除：先整库备份到回收目录，再按 id 删行（外键级联清理子表） ----------
fn trash_db(tool: &str, spec: &str) -> Result<String> {
    let Some((kind, id)) = spec.split_once(':') else {
        anyhow::bail!("无效的数据库删除目标: {}", spec);
    };
    if id.is_empty() {
        anyhow::bail!("数据库删除目标缺少会话 id");
    }
    let (db_path, db_label) = match kind {
        "zcode" => (format!("{}/.zcode/cli/db/db.sqlite", home_dir().display()), "db.sqlite"),
        "lobster" => (
            format!("{}/AppData/Roaming/LobsterAI/lobsterai.sqlite", home_dir().display()),
            "lobsterai.sqlite",
        ),
        _ => anyhow::bail!("未知的数据库类型: {}", kind),
    };
    if !Path::new(&db_path).exists() {
        anyhow::bail!("数据库不存在: {}", db_path);
    }
    // 0) id 消毒（防路径拼接注入）+ 先确认会话存在，避免无谓的大库备份
    if id.contains('/') || id.contains('\\') || id.contains("..") || id.chars().any(|c| c.is_control()) {
        anyhow::bail!("非法的会话 id");
    }
    let tbl = if kind == "zcode" { "session" } else { "cowork_sessions" };
    let con_chk = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    con_chk.busy_timeout(std::time::Duration::from_secs(3))?;
    let exists: i64 = con_chk.query_row(
        &format!("SELECT COUNT(*) FROM {} WHERE id = ?1", tbl),
        params![id],
        |r| r.get(0),
    )?;
    drop(con_chk);
    if exists == 0 {
        // 会话已被应用删除（或上一次删除已生效）：按“已失效”处理
        return Ok(String::new());
    }
    // 1) 一致性快照备份到回收目录（VACUUM INTO 事务级一致且压缩，源库写入中也可安全备份）
    let stamp = Local::now().format("%Y%m%d-%H%M%S-%3f");
    let dest_dir = out_dir().join("recycle").join(format!("{}-{}-{}", tool.replace(' ', "_"), stamp, id));
    fs::create_dir_all(&dest_dir)?;
    let snap = dest_dir.join(db_label);
    let con_bk = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    con_bk.busy_timeout(std::time::Duration::from_secs(3))?;
    con_bk.execute("VACUUM INTO ?1", params![snap.to_string_lossy().to_string()])?;
    drop(con_bk);
    // 2) 删行（事务）：session/cowork_sessions 主表删除后子表由外键级联清理；
    //    无外键的 input_history、dwf_run（ZCode）/scheduled_task_runs、im_session_mappings（LobsterAI）显式删除
    let con = Connection::open(&db_path)?;
    con.busy_timeout(std::time::Duration::from_secs(3))?;
    con.pragma_update(None, "foreign_keys", "ON")?;
    con.execute_batch("BEGIN IMMEDIATE")?;
    let del_result = (|| -> Result<usize> {
        let n = match kind {
            "zcode" => {
                let n = con.execute("DELETE FROM session WHERE id = ?1", params![id])?;
                con.execute("DELETE FROM input_history WHERE session_id = ?1", params![id])?;
                con.execute("DELETE FROM dwf_run WHERE parent_session_id = ?1", params![id])?;
                n
            }
            "lobster" => {
                let n = con.execute("DELETE FROM cowork_sessions WHERE id = ?1", params![id])?;
                con.execute("DELETE FROM scheduled_task_runs WHERE session_id = ?1", params![id])?;
                con.execute("DELETE FROM im_session_mappings WHERE cowork_session_id = ?1", params![id])?;
                n
            }
            _ => unreachable!(),
        };
        Ok(n)
    })();
    match del_result {
        Ok(n) => {
            con.execute_batch("COMMIT")?;
            if n == 0 {
                anyhow::bail!("会话不存在或已被删除: {}", id);
            }
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    // 注意：manifest 里存裸 id（不含 "zcode:"/"lobster:" 前缀），恢复时直接按 id 回插
    write_manifest(&dest_dir, tool, "db", id, Some(kind), Some(db_label))?;
    Ok(dest_dir.to_string_lossy().to_string())
}

fn fs_extra_like_move_dir(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let from = e.path();
        let to = dest.join(e.file_name());
        if from.is_dir() {
            fs_extra_like_move_dir(&from, &to)?;
            fs::remove_dir_all(&from)?;
        } else {
            fs::rename(&from, &to).or_else(|_| {
                fs::copy(&from, &to)?;
                fs::remove_file(&from)
            })?;
        }
    }
    fs::remove_dir(src)?;
    Ok(())
}

// ---------- 回收目录 / manifest / 恢复 ----------
static TRASH_SEQ: AtomicUsize = AtomicUsize::new(0);

fn unique_suffix(p: &Path) -> String {
    let n = TRASH_SEQ.fetch_add(1, Ordering::Relaxed);
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    format!("{}-{:03}-{}", Local::now().format("%Y%m%d-%H%M%S-%3f"), n, safe)
}

fn recycle_root() -> PathBuf {
    out_dir().join("recycle")
}

fn write_manifest(
    dir: &Path,
    tool: &str,
    kind: &str,
    target: &str,
    db: Option<&str>,
    snapshot: Option<&str>,
) -> Result<()> {
    let created = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let opt = |v: Option<&str>| match v {
        Some(s) => format!("\"{}\"", json_esc(s)),
        None => "null".to_string(),
    };
    let json = format!(
        "{{\n  \"tool\": \"{}\",\n  \"kind\": \"{}\",\n  \"target\": \"{}\",\n  \"db\": {},\n  \"snapshot\": {},\n  \"created\": \"{}\",\n  \"restored\": false\n}}\n",
        json_esc(tool),
        json_esc(kind),
        json_esc(target),
        opt(db),
        opt(snapshot),
        created
    );
    fs::write(dir.join("manifest.json"), json)?;
    Ok(())
}

fn restore_entry(name: &str) -> Result<String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.chars().any(|c| c.is_control())
    {
        anyhow::bail!("非法的回收条目名");
    }
    let dir = recycle_root().join(name);
    let mp = dir.join("manifest.json");
    if !mp.exists() {
        anyhow::bail!("找不到回收条目: {}", name);
    }
    let txt = fs::read_to_string(&mp)?;
    let mut v: Value = serde_json::from_str(&txt)?;
    if v.get("restored").and_then(|b| b.as_bool()).unwrap_or(false) {
        anyhow::bail!("该条目已恢复过");
    }
    let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let target = v.get("target").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if target.is_empty() {
        anyhow::bail!("manifest 缺少原始路径");
    }
    match kind.as_str() {
        "file" | "dir" => {
            let file_name = Path::new(&target)
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("无法解析原始文件名"))?;
            let src = dir.join(file_name);
            if !src.exists() {
                anyhow::bail!("回收目录里找不到被移走的文件/目录");
            }
            let dst = PathBuf::from(&target);
            if dst.exists() {
                anyhow::bail!("原位置已存在同名文件，未覆盖: {}", target);
            }
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&src, &dst)?;
        }
        "db" => {
            let db = v.get("db").and_then(|x| x.as_str()).unwrap_or("");
            let snap_name = v.get("snapshot").and_then(|x| x.as_str()).unwrap_or("");
            if snap_name.is_empty() {
                anyhow::bail!("manifest 缺少数据库快照名");
            }
            restore_db_row(db, &target, &dir.join(snap_name))?;
        }
        _ => anyhow::bail!("未知的回收条目类型: {}", kind),
    }
    v["restored"] = Value::Bool(true);
    fs::write(&mp, serde_json::to_string_pretty(&v)?)?;
    Ok(target)
}

fn restore_db_row(db_kind: &str, spec: &str, snap: &Path) -> Result<()> {
    if !snap.exists() {
        anyhow::bail!("数据库快照不存在: {}", snap.display());
    }
    let (db_path, main_tbl, children): (String, &str, &[(&str, &str)]) = match db_kind {
        "zcode" => (
            format!("{}/.zcode/cli/db/db.sqlite", home_dir().display()),
            "session",
            &[
                ("input_history", "session_id"),
                ("dwf_run", "parent_session_id"),
            ],
        ),
        "lobster" => (
            format!("{}/AppData/Roaming/LobsterAI/lobsterai.sqlite", home_dir().display()),
            "cowork_sessions",
            &[
                ("cowork_messages", "session_id"),
                ("scheduled_task_runs", "session_id"),
                ("im_session_mappings", "cowork_session_id"),
            ],
        ),
        _ => anyhow::bail!("未知的数据库类型: {}", db_kind),
    };
    if !Path::new(&db_path).exists() {
        anyhow::bail!("数据库不存在: {}", db_path);
    }
    let con = Connection::open(&db_path)?;
    con.busy_timeout(std::time::Duration::from_secs(3))?;
    con.pragma_update(None, "foreign_keys", "ON")?;
    con.execute(
        "ATTACH DATABASE ?1 AS bk",
        params![snap.to_string_lossy().to_string()],
    )?;
    let exists: i64 = con.query_row(
        &format!("SELECT COUNT(*) FROM {} WHERE id = ?1", main_tbl),
        params![spec],
        |r| r.get(0),
    )?;
    if exists > 0 {
        let _ = con.execute_batch("DETACH DATABASE bk");
        anyhow::bail!("目标会话已存在，未覆盖: {}", spec);
    }
    con.execute_batch("BEGIN IMMEDIATE")?;
    let res = (|| -> Result<()> {
        let inserted = con.execute(
            &format!(
                "INSERT OR REPLACE INTO main.{} SELECT * FROM bk.{} WHERE id = ?1",
                main_tbl, main_tbl
            ),
            params![spec],
        )?;
        if inserted == 0 {
            anyhow::bail!("快照中没有该会话，无法恢复: {}", spec);
        }
        for (tbl, key) in children {
            let in_snap: i64 = con.query_row(
                "SELECT COUNT(*) FROM bk.sqlite_master WHERE type='table' AND name=?1",
                params![tbl],
                |r| r.get(0),
            )?;
            if in_snap > 0 {
                con.execute(
                    &format!(
                        "INSERT OR REPLACE INTO main.{} SELECT * FROM bk.{} WHERE {} = ?1",
                        tbl, tbl, key
                    ),
                    params![spec],
                )?;
            }
        }
        Ok(())
    })();
    match res {
        Ok(()) => {
            con.execute_batch("COMMIT")?;
            let _ = con.execute_batch("DETACH DATABASE bk");
            Ok(())
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            let _ = con.execute_batch("DETACH DATABASE bk");
            Err(e)
        }
    }
}

fn recycle_page_html() -> String {
    let root = recycle_root();
    let mut items: Vec<(String, String, String, String, String, bool)> = vec![];
    if let Ok(rd) = fs::read_dir(&root) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let Ok(txt) = fs::read_to_string(p.join("manifest.json")) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&txt) else {
                continue;
            };
            items.push((
                e.file_name().to_string_lossy().to_string(),
                v.get("created").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                v.get("tool").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                v.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                v.get("target").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                v.get("restored").and_then(|x| x.as_bool()).unwrap_or(false),
            ));
        }
    }
    items.sort_by(|a, b| b.1.cmp(&a.1));
    let mut rows = String::new();
    for (name, created, tool, kind, target, restored) in &items {
        let status = if *restored {
            "<span class=\"ok\">已恢复</span>".to_string()
        } else {
            format!(
                "<button class=\"rs\" data-e=\"{n}\" onclick=\"restore(this)\">↩ 恢复</button><button class=\"pg\" data-e=\"{n}\" onclick=\"purgeEntry(this)\">🗑 删除</button>",
                n = esc(name)
            )
        };
        rows.push_str(&format!(
            "<tr><td class=\"t\">{}</td><td>{}</td><td>{}</td><td class=\"f\">{}</td><td class=\"act\">{}</td></tr>\n",
            esc(created),
            esc(tool),
            esc(kind),
            esc(target),
            status
        ));
    }
    if rows.is_empty() {
        rows.push_str("<tr><td colspan=\"5\" class=\"empty\">回收站是空的</td></tr>\n");
    }
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>回收站 · ai-qa-report</title>
<style>
*{{margin:0;padding:0;box-sizing:border-box}}
body{{font-family:system-ui,-apple-system,"Segoe UI",sans-serif;background:#0f1117;color:#e5e7eb;padding:32px 20px}}
.wrap{{max-width:1100px;margin:0 auto}}
h1{{font-size:24px;margin-bottom:6px}}
.sub{{color:#9ca3af;font-size:13px;margin-bottom:20px}}
a{{color:#67e8f9;text-decoration:none}}
table{{width:100%;border-collapse:collapse;background:#12141c;border-radius:12px;overflow:hidden;border:1px solid #232838}}
th{{background:#1a1e2b;text-align:left;padding:10px 12px;font-size:12px;color:#9ca3af}}
td{{padding:9px 12px;border-top:1px solid #1d2230;font-size:13px;vertical-align:top}}
.t{{white-space:nowrap;color:#9ca3af;font-size:12px}}
.f{{color:#67e8f9;font-size:12px;word-break:break-all;font-family:Consolas,monospace}}
.act{{width:90px;text-align:center}}
.rs{{background:#14532d;border:1px solid #22c55e;color:#bbf7d0;border-radius:6px;cursor:pointer;font-size:12px;padding:3px 10px}}
.rs:hover{{background:#166534;color:#fff}}
.pg{{background:#3f1d1d;border:1px solid #f87171;color:#fecaca;border-radius:6px;cursor:pointer;font-size:12px;padding:3px 10px;margin-left:6px}}
.pg:hover{{background:#7f1d1d;color:#fff}}
.ok{{color:#22c55e;font-size:12px}}
.empty{{text-align:center;color:#6b7280;padding:24px}}
.clr{{background:#3f1d1d;border:1px solid #f87171;color:#fecaca;border-radius:8px;padding:5px 12px;cursor:pointer;font-size:12px;margin-left:10px}}
.clr:hover{{background:#7f1d1d;color:#fff}}
.mask{{position:fixed;inset:0;background:rgba(0,0,0,.62);display:none;align-items:center;justify-content:center;z-index:9}}
.mask.on{{display:flex}}
.dlg{{background:#171a23;border:1px solid #2a3040;border-radius:12px;padding:20px;width:380px;max-width:92vw}}
.dt{{font-size:16px;margin-bottom:8px;color:#fff}}
.dd{{color:#9ca3af;font-size:12px;line-height:1.7;margin-bottom:10px}}
.dd b{{color:#fca5a5}}
#cf{{width:100%;padding:8px 10px;border-radius:8px;border:1px solid #2a3040;background:#0f1117;color:#e5e7eb;margin-bottom:8px;font-size:13px}}
.dmsg{{color:#fbbf24;font-size:12px;min-height:16px;margin-bottom:10px}}
.dbtns{{display:flex;gap:10px;justify-content:flex-end}}
.dbtns .rs,.dbtns .clr{{margin-left:0}}
.foot{{margin-top:16px;color:#6b7280;font-size:12px;line-height:1.8}}
</style></head><body><div class="wrap">
<h1>🗑 回收站</h1>
<div class="sub">共 {} 条删除记录 ｜ 目录：{} ｜ <a href="/">← 返回报告</a><button class="clr" onclick="clearAll()">清空回收站</button></div>
<table><thead><tr><th>删除时间</th><th>工具</th><th>类型</th><th>原始位置</th><th>操作</th></tr></thead>
<tbody>{}</tbody></table>
<div class="foot">文件型会话：恢复 = 从回收目录移回原位置，不覆盖已存在的同名文件。<br>
数据库型（ZCode/LobsterAI）：恢复 = 从整库快照中把该会话及其子表记录插回数据库；已存在同 id 时会拒绝覆盖。<br>
单行“删除”或“清空”= 移入<b>系统回收站/废纸篓</b>（仍可恢复），本工具不做不可逆删除。</div>
<div class="mask" id="mask">
  <div class="dlg">
    <div class="dt">清空回收站</div>
    <div class="dd">所有条目会移入 Windows 系统回收站（仍可恢复）。<br>请输入 <b>清空</b> 两个字确认：</div>
    <input id="cf" placeholder="输入 清空" autocomplete="off">
    <div class="dmsg" id="dmsg"></div>
    <div class="dbtns">
      <button class="rs" onclick="hideClear()">取消</button>
      <button class="clr" onclick="doClear()">确认清空</button>
    </div>
  </div>
</div>
</div>
<script>
async function restore(btn){{
  var e=btn.dataset.e;
  if(!confirm('恢复 '+e+' ？'))return;
  btn.disabled=true;btn.textContent='…';
  try{{
    var r=await fetch('/restore/'+encodeURIComponent(e),{{method:'POST'}});
    var j=await r.json();
    if(j.ok){{location.reload();}}else{{alert('恢复失败: '+(j.error||'未知'));btn.disabled=false;btn.textContent='↩ 恢复';}}
  }}catch(err){{alert('请求失败: '+err);btn.disabled=false;btn.textContent='↩ 恢复';}}
}}
function clearAll(){{
  var cf=document.getElementById('cf');
  cf.value='';
  document.getElementById('dmsg').textContent='';
  document.getElementById('mask').classList.add('on');
  cf.focus();
}}
function hideClear(){{
  document.getElementById('mask').classList.remove('on');
}}
async function doClear(){{
  var v=document.getElementById('cf').value.trim();
  if(v!=='清空'){{
    document.getElementById('dmsg').textContent='请输入“清空”两个字';
    return;
  }}
  document.getElementById('dmsg').textContent='正在移入系统回收站…';
  try{{
    var r=await fetch('/clear',{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify({{confirm:'清空'}})}});
    var j=await r.json();
    if(j.ok){{
      var msg='已移入系统回收站 '+j.moved+' 项';
      if(j.errors&&j.errors.length)msg+='，失败 '+j.errors.length+' 项：'+j.errors.slice(0,6).join('；');
      document.getElementById('dmsg').textContent=msg+'，正在刷新…';
      setTimeout(function(){{location.reload()}},800);
    }}else{{
      document.getElementById('dmsg').textContent='清空失败: '+(j.error||'未知');
    }}
  }}catch(e){{
    document.getElementById('dmsg').textContent='请求失败: '+e;
  }}
}}
function purgeEntry(btn){{
  if(!btn.dataset.armed){{
    btn.dataset.armed='1';
    btn.textContent='确认删除?';
    setTimeout(function(){{
      if(!btn.disabled){{ btn.dataset.armed=''; btn.textContent='🗑 删除'; }}
    }},3000);
    return;
  }}
  btn.disabled=true;
  btn.textContent='…';
  fetch('/purge/'+encodeURIComponent(btn.dataset.e),{{method:'POST'}})
    .then(function(r){{ return r.json(); }})
    .then(function(j){{
      if(j.ok){{ location.reload(); }}
      else {{ alert('删除失败: '+(j.error||'未知')); btn.disabled=false; btn.textContent='🗑 删除'; }}
    }})
    .catch(function(e){{
      alert('请求失败: '+e); btn.disabled=false; btn.textContent='🗑 删除';
    }});
}}
</script></body></html>"#,
        items.len(),
        esc(&root.to_string_lossy()),
        rows
    )
}

// ---------- 清空回收站：移入 Windows 系统回收站（仍可恢复，不做不可逆删除） ----------
fn purge_entry(name: &str) -> Result<String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.chars().any(|c| c.is_control())
    {
        anyhow::bail!("非法的回收条目名");
    }
    let dir = recycle_root().join(name);
    if !dir.exists() {
        anyhow::bail!("回收条目不存在: {}", name);
    }
    send_to_os_recycle(&dir)?;
    Ok(name.to_string())
}

fn clear_recycle() -> Result<(usize, Vec<String>)> {
    let root = recycle_root();
    let mut moved = 0usize;
    let mut errors: Vec<String> = vec![];
    if !root.exists() {
        return Ok((0, errors));
    }
    for e in fs::read_dir(&root)? {
        let e = e?;
        let p = e.path();
        match send_to_os_recycle(&p) {
            Ok(()) => moved += 1,
            Err(err) => errors.push(format!(
                "{}: {}",
                e.file_name().to_string_lossy(),
                err
            )),
        }
    }
    Ok((moved, errors))
}

#[cfg(target_os = "windows")]
fn send_to_os_recycle(path: &Path) -> Result<()> {
    let p = path.to_string_lossy().replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName Microsoft.VisualBasic; $p='{p}'; if (Test-Path -LiteralPath $p) {{ if ((Get-Item -LiteralPath $p).PSIsContainer) {{ [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($p,'OnlyErrorDialogs','SendToRecycleBin') }} else {{ [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($p,'OnlyErrorDialogs','SendToRecycleBin') }} }}"
    );
    let out = std::process::Command::new("powershell.exe")
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(&script)
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "移入系统回收站失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn send_to_os_recycle(path: &Path) -> Result<()> {
    let trash = home_dir().join(".Trash");
    fs::create_dir_all(&trash)?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "ai-qa-report-item".into());
    let mut dest = trash.join(&name);
    if dest.exists() {
        dest = trash.join(format!(
            "{}-{}",
            name,
            Local::now().format("%Y%m%d-%H%M%S")
        ));
    }
    fs::rename(path, &dest)
        .map_err(|e| anyhow::anyhow!("移入 ~/.Trash 失败: {}", e))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn send_to_os_recycle(path: &Path) -> Result<()> {
    if let Ok(out) = std::process::Command::new("gio")
        .arg("trash")
        .arg("--")
        .arg(path)
        .output()
    {
        if out.status.success() {
            return Ok(());
        }
    }
    match std::process::Command::new("trash-put").arg(path).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => anyhow::bail!(
            "移入系统回收站失败（gio/trash-put）: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(_) => anyhow::bail!("未找到 gio 或 trash-put，无法移入系统回收站"),
    }
}

// ---------- serve 模式：极简 HTTP（std only, 无依赖） ----------
use std::io::{Write as IoWrite};
use std::net::{TcpListener, TcpStream};

fn serve(port: u16) -> Result<()> {
    run_build()?;
    let report_path = std::sync::Arc::new(out_dir().join("AI问答记录梳理报告.html"));

    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))?;
    println!("服务已启动: http://127.0.0.1:{} （Ctrl+C 停止）", port);
    println!("页面删除按钮可用；删除 = 移入 {} (可恢复)", out_dir().join("recycle").display());

    let build_lock = std::sync::Arc::new(std::sync::Mutex::new(()));
    let build_lock_c = build_lock.clone();
    std::thread::spawn(move || {
        // 每 60s 重建一次报告，让删除后其他打开的页面也保持最新
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            let _g = build_lock_c.lock().unwrap();
            let _ = run_build();
        }
    });
    for stream in listener.incoming() {
        let mut stream = match stream { Ok(s) => s, Err(_) => continue };
        let build_lock = build_lock.clone();
        let report_path = report_path.clone();
        std::thread::spawn(move || {
            let _ = handle_conn(&mut stream, &build_lock, &report_path);
        });
    }
    Ok(())
}

fn read_http_request(stream: &mut TcpStream) -> Result<String> {
    let mut data: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..pos]).to_string();
            let mut need = 0usize;
            for line in head.lines().skip(1) {
                let ll = line.to_ascii_lowercase();
                if let Some(v) = ll.strip_prefix("content-length:") {
                    need = v.trim().parse().unwrap_or(0);
                }
            }
            if data.len() >= pos + 4 + need {
                break;
            }
        }
        if data.len() > 4 * 1024 * 1024 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&data).to_string())
}

fn respond(stream: &mut TcpStream, status: &str, ctype: &str, body: &str) -> Result<()> {
    let resp = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n{}",
        status,
        ctype,
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes())?;
    Ok(())
}

fn handle_conn(stream: &mut TcpStream, build_lock: &std::sync::Mutex<()>, report_path: &Path) -> Result<()> {
    let req = read_http_request(stream)?;
    let (head, body) = req.split_once("\r\n\r\n").unwrap_or((req.as_str(), ""));
    let first_line = head.lines().next().unwrap_or("");
    let mut parts = first_line.split(' ');
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");

    // 本机 CSRF/DNS-rebinding 防护：Host 必须是 127.0.0.1/localhost，带 Origin 头时必须同源
    let mut host_ok = false;
    let mut origin_ok = true; // 无 Origin 头（直接访问/同源请求）视为通过
    for line in head.lines().skip(1) {
        if line.is_empty() {
            break;
        }
        let ll = line.to_ascii_lowercase();
        if let Some(h) = ll.strip_prefix("host:") {
            let h = h.trim();
            host_ok = h.starts_with("127.0.0.1") || h.starts_with("localhost");
        } else if let Some(o) = ll.strip_prefix("origin:") {
            let o = o.trim();
            origin_ok = o == "http://127.0.0.1" || o.starts_with("http://127.0.0.1:")
                || o == "http://localhost" || o.starts_with("http://localhost:");
        }
    }
    if !host_ok || !origin_ok {
        return respond(
            stream,
            "403 FORBIDDEN",
            "application/json; charset=utf-8",
            "{\"ok\":false,\"error\":\"forbidden host/origin\"}",
        );
    }

    if method == "GET" && (path == "/" || path.starts_with("/?")) {
        let html = fs::read_to_string(report_path).unwrap_or_else(|_| "<h1>报告未生成</h1>".into());
        return respond(stream, "200 OK", "text/html; charset=utf-8", &html);
    }

    if method == "GET" && path.starts_with("/recycle") {
        return respond(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            &recycle_page_html(),
        );
    }

    if method == "POST" && path.starts_with("/delete_batch") {
        let keys: Vec<String> = serde_json::from_str(body).unwrap_or_default();
        if keys.is_empty() {
            return respond(
                stream,
                "200 OK",
                "application/json; charset=utf-8",
                "{\"ok\":true,\"deleted\":0,\"errors\":[]}",
            );
        }
        let raw = fs::read_to_string(out_dir().join("delmap.tsv")).unwrap_or_default();
        let lines: Vec<&str> = raw.lines().collect();
        let (mut done, mut skipped, mut errors) = (0usize, 0usize, Vec::<String>::new());
        for k in keys.iter().take(500) {
            if let Some(line) = lines.iter().find(|l| **l == k.as_str()) {
                let mut it = line.splitn(2, '\t');
                let tool = it.next().unwrap_or("").to_string();
                let target = it.next().unwrap_or("").to_string();
                if target.is_empty() {
                    errors.push(format!("{}: 不支持删除", tool));
                    continue;
                }
                match trash_target(&tool, &target) {
                    Ok(d) if d.is_empty() => skipped += 1,
                    Ok(_) => done += 1,
                    Err(e) => errors.push(format!("{}: {}", tool, e)),
                }
            } else {
                let short: String = k.chars().take(70).collect();
                errors.push(format!("未找到条目: {}", short));
            }
        }
        if done > 0 || skipped > 0 {
            let _g = build_lock.lock().unwrap();
            let _ = run_build();
        }
        let err_json = errors
            .iter()
            .map(|e| format!("\"{}\"", json_esc(e)))
            .collect::<Vec<_>>()
            .join(",");
        let out = format!(
            "{{\"ok\":true,\"deleted\":{},\"skipped\":{},\"errors\":[{}]}}",
            done, skipped, err_json
        );
        return respond(stream, "200 OK", "application/json; charset=utf-8", &out);
    }

    if method == "POST" && path.starts_with("/delete/") {
        // 每次请求重读 delmap.tsv（不缓存），并用页面携带的整行 key 校验行号，防重建后错位删错
        let (ppath, query) = match path.split_once('?') {
            Some((a, b)) => (a, b),
            None => (path, ""),
        };
        let idx: usize = ppath.rsplit('/').next().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
        let want_k = query.split('&').find_map(|kv| kv.strip_prefix("k=")).map(percent_decode);
        let result = (|| -> Result<String> {
            let raw = fs::read_to_string(out_dir().join("delmap.tsv")).unwrap_or_default();
            let Some((tool, target)) = raw.lines().nth(idx).map(|l| {
                let mut it = l.splitn(2, '\t');
                (
                    it.next().unwrap_or("").to_string(),
                    it.next().unwrap_or("").to_string(),
                )
            }) else {
                anyhow::bail!("无效索引 {}（列表可能已更新，请刷新页面）", idx);
            };
            match &want_k {
                Some(k) if *k == format!("{}\t{}", tool, target) => {}
                _ => anyhow::bail!("列表已更新，行号与内容不匹配；请刷新页面后重试"),
            }
            if target.is_empty() {
                anyhow::bail!("该条目({})不支持删除", tool);
            }
            trash_target(&tool, &target)
        })();
        // 删除后重建报告（保持页面与磁盘一致；与 60s 定时重建互斥，防写坏文件）
        if result.is_ok() {
            let _g = build_lock.lock().unwrap();
            let _ = run_build();
        }
        let out = match result {
            Ok(dest) if dest.is_empty() => {
                "{\"ok\":true,\"skipped\":true,\"note\":\"目标已不存在（可能已被应用清理），列表已刷新\"}".to_string()
            }
            Ok(dest) => format!("{{\"ok\":true,\"recycle\":\"{}\"}}", json_esc(&dest.replace('\\', "/"))),
            Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", json_esc(&e.to_string())),
        };
        return respond(stream, "200 OK", "application/json; charset=utf-8", &out);
    }

    if method == "POST" && path.starts_with("/reveal/") {
        let (ppath, query) = match path.split_once('?') {
            Some((a, b)) => (a, b),
            None => (path, ""),
        };
        let idx: usize = ppath
            .rsplit('/')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(usize::MAX);
        let want_k = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("k="))
            .map(percent_decode);
        let result = (|| -> Result<String> {
            let raw = fs::read_to_string(out_dir().join("delmap.tsv")).unwrap_or_default();
            let Some((tool, target)) = raw.lines().nth(idx).map(|l| {
                let mut it = l.splitn(2, '\t');
                (
                    it.next().unwrap_or("").to_string(),
                    it.next().unwrap_or("").to_string(),
                )
            }) else {
                anyhow::bail!("无效索引 {}（列表可能已更新，请刷新页面）", idx);
            };
            match &want_k {
                Some(k) if *k == format!("{}\t{}", tool, target) => {}
                _ => anyhow::bail!("列表已更新，行号与内容不匹配；请刷新页面后重试"),
            }
            if target.is_empty() {
                anyhow::bail!("该条目({})没有可打开的本地位置", tool);
            }
            reveal_target(&tool, &target)
        })();
        let out = match result {
            Ok(p) => format!(
                "{{\"ok\":true,\"path\":\"{}\"}}",
                json_esc(&p.replace('\\', "/"))
            ),
            Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", json_esc(&e.to_string())),
        };
        return respond(stream, "200 OK", "application/json; charset=utf-8", &out);
    }

    if method == "POST" && path.starts_with("/purge/") {
        let entry = percent_decode(path.rsplit('/').next().unwrap_or(""));
        let out = match purge_entry(&entry) {
            Ok(n) => format!("{{\"ok\":true,\"purged\":\"{}\"}}", json_esc(&n)),
            Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", json_esc(&e.to_string())),
        };
        return respond(stream, "200 OK", "application/json; charset=utf-8", &out);
    }

    if method == "POST" && path.starts_with("/clear") {
        if !body.contains("清空") {
            return respond(
                stream,
                "400 BAD REQUEST",
                "application/json; charset=utf-8",
                "{\"ok\":false,\"error\":\"需要确认文本：清空\"}",
            );
        }
        let out = match clear_recycle() {
            Ok((moved, errors)) => {
                let err_json = errors
                    .iter()
                    .map(|e| format!("\"{}\"", json_esc(e)))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{{\"ok\":true,\"moved\":{},\"errors\":[{}]}}", moved, err_json)
            }
            Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", json_esc(&e.to_string())),
        };
        return respond(stream, "200 OK", "application/json; charset=utf-8", &out);
    }

    if method == "POST" && path.starts_with("/restore/") {
        let entry = percent_decode(path.rsplit('/').next().unwrap_or(""));
        let out = match restore_entry(&entry) {
            Ok(p) => format!("{{\"ok\":true,\"restored\":\"{}\"}}", json_esc(&p.replace('\\', "/"))),
            Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", json_esc(&e.to_string())),
        };
        return respond(stream, "200 OK", "application/json; charset=utf-8", &out);
    }

    respond(
        stream,
        "404 NOT FOUND",
        "application/json; charset=utf-8",
        "{\"ok\":false,\"error\":\"not found\"}",
    )
}

fn json_esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}
