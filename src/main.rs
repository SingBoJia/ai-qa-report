use anyhow::Result;
use chrono::{DateTime, Local, TimeZone};
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

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
    let mut stmt = con.prepare(
        "SELECT title, directory, time_created FROM session ORDER BY time_created",
    )?;
    let mut rows = vec![];
    let iter = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<i64>>(2)?,
        ))
    })?;
    for r in iter {
        let (title, dir, ts) = r?;
        rows.push(Row {
            tool: "ZCode",
            time: ts.map(ms2str).unwrap_or_default(),
            what: summ(&title.unwrap_or_default()),
            folder: "~/.zcode/cli/db/db.sqlite".into(),
            detail: format!("工作目录: {}", dir.unwrap_or_else(|| "-".into())),
            del_id: String::new(),
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
                tool: Box::leak(tool.to_string().into_boxed_str()),
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
            del_id: id.unwrap_or_default(),
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
                "<button class=\"del\" onclick=\"delRow({},this)\" title=\"移入回收目录（可恢复）\">✕</button>",
                idx
            )
        };
        trs.push_str(&format!(
            "<tr data-tool=\"{}\" data-idx=\"{}\"><td class=\"t\">{}</td><td><span class=\"badge\" style=\"background:{}\">{}</span></td><td class=\"w\">{}<div class=\"d\">{}</div></td><td class=\"f\">{}</td><td class=\"delc\">{}</td></tr>\n",
            esc(r.tool), idx, esc(&r.time[..r.time.len().min(16)]), color, esc(r.tool),
            esc(&r.what), esc(&r.detail), esc(&r.folder), del_btn
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
}

fn main() -> Result<()> {
    // serve 模式: ai-qa-report.exe serve [port]  → 启动本地服务，页面删除按钮可用
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "serve" {
        let port: u16 = args.get(2).map(|p| p.parse().unwrap_or(8765)).unwrap_or(8765);
        return serve(port);
    }
    run_build()?;
    // 构建后自动以 serve 模式拉起本地服务（后台不阻塞由用户决定）
    Ok(())
}

fn run_build() -> Result<()> {
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

// ---------- 删除：把目标移入回收目录（不真删，可恢复） ----------
fn trash_target(tool: &str, target: &str) -> Result<String> {
    let p = PathBuf::from(target);
    if !p.exists() {
        anyhow::bail!("目标不存在: {}", target);
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
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let dest_dir = out_dir().join("recycle").join(format!("{}-{}", tool.replace(' ', "_"), stamp));
    fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(p.file_name().unwrap_or_default());
    if p.is_dir() {
        fs_extra_like_move_dir(&p, &dest)?;
    } else {
        fs::rename(&p, &dest).or_else(|_| {
            fs::copy(&p, &dest)?;
            fs::remove_file(&p)
        })?;
    }
    // Antigravity: 同时清掉摘要索引里的记录由应用自行处理；这里额外备份摘要文件
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

// ---------- serve 模式：极简 HTTP（std only, 无依赖） ----------
use std::io::{Write as IoWrite};
use std::net::{TcpListener, TcpStream};

fn serve(port: u16) -> Result<()> {
    run_build()?;
    let delmap_raw = fs::read_to_string(out_dir().join("delmap.tsv")).unwrap_or_default();
    let delmap: Vec<(String, String)> = delmap_raw
        .lines()
        .map(|l| {
            let mut it = l.splitn(2, '\t');
            (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string())
        })
        .collect();
    let report_path = std::sync::Arc::new(out_dir().join("AI问答记录梳理报告.html"));

    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))?;
    println!("服务已启动: http://127.0.0.1:{} （Ctrl+C 停止）", port);
    println!("页面删除按钮可用；删除 = 移入 {} (可恢复)", out_dir().join("recycle").display());

    let delmap = std::sync::Arc::new(std::sync::Mutex::new(delmap));
    let report_path_c = report_path.clone();
    std::thread::spawn(move || {
        // 每 60s 重建一次报告，让删除后其他打开的页面也保持最新
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            let _ = run_build();
        }
    });
    let _ = report_path_c;
    for stream in listener.incoming() {
        let mut stream = match stream { Ok(s) => s, Err(_) => continue };
        let delmap = delmap.clone();
        let report_path = report_path.clone();
        std::thread::spawn(move || {
            let _ = handle_conn(&mut stream, &delmap, &report_path);
        });
    }
    Ok(())
}

fn handle_conn(stream: &mut TcpStream, delmap: &std::sync::Mutex<Vec<(String, String)>>, report_path: &Path) -> Result<()> {
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf)?;
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let first_line = req.lines().next().unwrap_or("");
    let mut parts = first_line.split(' ');
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");

    if method == "GET" && (path == "/" || path.starts_with("/?")) {
        let html = fs::read_to_string(report_path).unwrap_or_else(|_| "<h1>报告未生成</h1>".into());
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
            html.len(), html
        );
        stream.write_all(resp.as_bytes())?;
    } else if method == "POST" && path.starts_with("/delete/") {
        let idx: usize = path.rsplit('/').next().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
        let result = (|| -> Result<String> {
            let map = delmap.lock().unwrap();
            let Some((tool, target)) = map.get(idx) else {
                anyhow::bail!("无效索引 {}", idx);
            };
            if target.is_empty() {
                anyhow::bail!("该条目({})不支持删除（记录在数据库/加密库内）", tool);
            }
            trash_target(tool, target)
        })();
        // 删除后重建报告（保持页面与磁盘一致）
        if result.is_ok() {
            let _ = run_build();
        }
        let body = match result {
            Ok(dest) => format!("{{\"ok\":true,\"recycle\":\"{}\"}}", dest.replace('\\', "/")),
            Err(e) => format!("{{\"ok\":false,\"error\":\"{}\"}}", esc(&e.to_string())),
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        );
        stream.write_all(resp.as_bytes())?;
    } else {
        let body = "{\"ok\":false,\"error\":\"not found\"}";
        let resp = format!(
            "HTTP/1.1 404 NOT FOUND\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        );
        stream.write_all(resp.as_bytes())?;
    }
    Ok(())
}
