//! Every setting there is, for the Settings page's Everything tab: read from
//! config.example.toml (built in), which has each section and key with a
//! sample value and its comment. Each field gets its type from the sample,
//! its help from the comment, and its value from config.toml now. Keys in
//! config.toml the example doesn't know are listed too (lyra ignores them).

use serde_json::{Value, json};

const EXAMPLE: &str = include_str!("../config.example.toml");

/// One setting.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// "url" at the top; "planner.day_end"; "memory.context.max_tokens".
    pub path: String,
    pub section: String,
    pub key: String,
    /// bool, int, real, text, list, table (an inline table: shown, edited as TOML).
    pub kind: &'static str,
    /// The example's value (a hint, not always lyra's default).
    pub sample: toml::Value,
    pub help: String,
}

/// One section, with what the example says about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub name: String,
    pub help: String,
    /// `[[name]]`: a list of tables (one per provider): shown, edited in the file.
    pub list: bool,
}

/// Settings kept to config.toml: they widen what runs on the server without
/// asking, or run programs (approvals stay in Rust, set by whoever has the file).
const FILE_ONLY: &[(&str, &str)] = &[
    ("capabilities.openapi", "an API's connection: edit it in config.toml"),
    ("capabilities.mcp", "it starts a program on the server: edit it in config.toml"),
    ("capabilities.policy", "what tools may do without asking: edit it in config.toml"),
    ("system.shell", "the shell commands run in: edit it in config.toml"),
    ("system.allow_commands", "commands that run without asking: change them on the Machines page (Rules)"),
    ("system.write_roots", "folders written without asking: change them on the Machines page (Rules)"),
    ("system.deny_paths", "change them on the Machines page (Rules)"),
    ("system.ssh_hosts", "change them on the Machines page (Rules)"),
    ("coding.allow_push", "coding agents never push from the app: edit it in config.toml"),
    ("sandbox.allow_lan", "lets pages reach your network: edit it in config.toml"),
];

/// Settings that can lock you out, or move lyra's data: asked again first.
const CAREFUL: &[(&str, &str)] = &[
    ("web.listen", "Where lyra listens: wrong, and this app can't reach lyra after a restart (fix it in config.toml then)."),
    ("web.public_url", "The address phones use: wrong, and installing the app and notifications stop working."),
    ("web.entra", "Signing in with Microsoft: wrong, and nobody can sign in (paired devices keep working)."),
    ("memory.backend", "Where memories are kept: a different store starts empty."),
    ("memory.path", "Where memories are kept: a different place starts empty."),
    ("backup.dir", "Where backups go: older ones stay where they were."),
    ("embedding.model", "A different embedding model means memories are embedded again."),
];

/// Settings that take effect only when lyra starts again.
const AT_START: &[&str] = &["web.listen", "web.entra", "memory.backend", "memory.path", "web.public_url"];

fn rule<'a>(list: &'a [(&str, &str)], path: &str) -> Option<&'a str> {
    list.iter().find(|(p, _)| path == *p || path.starts_with(&format!("{p}."))).map(|(_, why)| *why)
}

pub fn file_only(path: &str) -> Option<&'static str> {
    rule(FILE_ONLY, path)
}

pub fn careful(path: &str) -> Option<&'static str> {
    rule(CAREFUL, path)
}

pub fn at_start(path: &str) -> bool {
    AT_START.iter().any(|p| path == *p || path.starts_with(&format!("{p}.")))
}

/// A key's value is secret (shown as set or not, never sent to the page).
pub fn secret(path: &str) -> bool {
    let key = path.rsplit('.').next().unwrap_or(path);
    key == "api_key" || key.ends_with("_token") || key == "password"
}

fn kind(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::Boolean(_) => "bool",
        toml::Value::Integer(_) => "int",
        toml::Value::Float(_) => "real",
        toml::Value::Array(_) => "list",
        toml::Value::Table(_) => "table",
        _ => "text",
    }
}

/// `key = value   # comment`: the value as TOML, and the comment.
fn setting(line: &str) -> Option<(String, toml::Value, String)> {
    let (key, rest) = line.split_once(" = ")?;
    let key = key.trim();
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '"' || c == '.') {
        return None;
    }
    // The shortest start of the rest that reads as a value; what follows a `#` after it is the comment.
    let mut cut = rest.len();
    let mut found = None;
    loop {
        let candidate = rest[..cut].trim();
        if let Ok(t) = toml::from_str::<toml::Table>(&format!("v = {candidate}"))
            && let Some(v) = t.get("v")
        {
            found = Some((v.clone(), rest[cut..].trim().trim_start_matches('#').trim().to_string()));
        }
        match rest[..cut].rfind(" #") {
            Some(i) => cut = i,
            None => break,
        }
    }
    let (value, comment) = found?;
    Some((key.trim_matches('"').to_string(), value, comment))
}

/// The example's sections and fields, in its order.
pub fn schema() -> (Vec<Section>, Vec<Field>) {
    let mut sections = vec![Section { name: String::new(), help: "The chat model, its prices, and lyra's internal calls.".into(), list: false }];
    let mut fields: Vec<Field> = Vec::new();
    let (mut section, mut list) = (String::new(), false);
    let mut prose: Vec<String> = Vec::new();
    for raw in EXAMPLE.lines() {
        let line = raw.strip_prefix("# ").map(str::trim_end).or_else(|| (!raw.starts_with('#')).then_some(raw.trim_end()));
        let Some(line) = line else {
            if raw.trim() == "#" {
                prose.clear();
            }
            continue;
        };
        if line.trim().is_empty() {
            prose.clear();
            continue;
        }
        let head = line.split('#').next().unwrap_or("").trim();
        if let Some(name) = head.strip_prefix("[[").and_then(|h| h.strip_suffix("]]")) {
            (section, list) = (name.trim().to_string(), true);
            if !sections.iter().any(|s| s.name == section) {
                sections.push(Section { name: section.clone(), help: prose.join(" "), list: true });
            }
            prose.clear();
            continue;
        }
        if let Some(name) = head.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            (section, list) = (name.trim().to_string(), false);
            let comment = line.split_once('#').map(|(_, c)| c.trim().to_string()).unwrap_or_default();
            let help = [prose.join(" "), comment].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
            match sections.iter_mut().find(|s| s.name == section) {
                Some(s) if s.help.is_empty() => s.help = help,
                Some(_) => {}
                None => sections.push(Section { name: section.clone(), help, list: false }),
            }
            prose.clear();
            continue;
        }
        if let Some((key, sample, comment)) = setting(line) {
            if list {
                prose.clear();
                continue;
            }
            let path = if section.is_empty() { key.clone() } else { format!("{section}.{key}") };
            if !fields.iter().any(|f| f.path == path) {
                let help = if comment.is_empty() { prose.join(" ") } else { comment };
                fields.push(Field { path, section: section.clone(), key, kind: kind(&sample), sample, help });
            }
            prose.clear();
            continue;
        }
        // A sentence of the example's comments: it explains what comes next.
        if raw.starts_with('#') {
            prose.push(line.trim().to_string());
        }
    }
    (sections, fields)
}

fn json_of(v: &toml::Value) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// The value at `path` in a config file's table.
pub fn get<'a>(doc: &'a toml::Table, path: &str) -> Option<&'a toml::Value> {
    let mut parts: Vec<&str> = path.split('.').collect();
    let last = parts.pop()?;
    let mut t = doc;
    for p in parts {
        t = t.get(p)?.as_table()?;
    }
    t.get(last)
}

/// Every key in config.toml that the example doesn't have (lyra ignores them).
fn unknown(doc: &toml::Table, known: &[Field], sections: &[Section]) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(t: &toml::Table, at: &str, out: &mut Vec<String>) {
        for (k, v) in t {
            let path = if at.is_empty() { k.clone() } else { format!("{at}.{k}") };
            match v {
                toml::Value::Table(inner) if !inner.is_empty() => walk(inner, &path, out),
                _ => out.push(path),
            }
        }
    }
    walk(doc, "", &mut out);
    out.retain(|p| {
        let section = p.rsplit_once('.').map_or("", |(s, _)| s);
        // Under a list of tables, or a section of free-form names ([groups], half-lives): fine.
        let free = sections.iter().any(|s| (s.list || matches!(s.name.as_str(), "groups" | "memory.half_life_days" | "capabilities.policy")) && (section == s.name || p.starts_with(&format!("{}.", s.name))));
        !free && !known.iter().any(|f| f.path == *p)
    });
    out
}

/// The Everything tab: each section and its fields with their values now.
pub fn page(text: &str) -> Value {
    let doc: toml::Table = match toml::from_str(text) {
        Ok(d) => d,
        Err(e) => return json!({ "error": format!("config.toml doesn't read: {e}") }),
    };
    let (sections, fields) = schema();
    let field_json = |f: &Field| {
        let now = get(&doc, &f.path);
        let mut v = json!({
            "path": f.path, "key": f.key, "kind": f.kind, "help": f.help,
            "sample": if secret(&f.path) { Value::Null } else { json_of(&f.sample) },
            "set": now.is_some(),
            "value": match now {
                Some(_) if secret(&f.path) => Value::Null,
                Some(v) => json_of(v),
                None => Value::Null,
            },
        });
        if secret(&f.path) {
            v["secret"] = json!(true);
            v["has_value"] = json!(now.and_then(toml::Value::as_str).is_some_and(|s| !s.trim().is_empty()));
        }
        if let Some(why) = file_only(&f.path) {
            v["file_only"] = json!(why);
        }
        if let Some(why) = careful(&f.path) {
            v["careful"] = json!(why);
        }
        if at_start(&f.path) {
            v["at_start"] = json!(true);
        }
        v
    };
    let mut out: Vec<Value> = sections
        .iter()
        .map(|s| {
            let mine: Vec<Value> = fields.iter().filter(|f| f.section == s.name).map(field_json).collect();
            let mut v = json!({ "name": s.name, "title": if s.name.is_empty() { "Chat model".to_string() } else { format!("[{}]", s.name) }, "help": s.help, "fields": mine, "list": s.list, "set": s.name.is_empty() || get(&doc, &s.name).is_some() || doc.get(s.name.split('.').next().unwrap_or("")).is_some() });
            // A list of tables (OpenAPI, MCP): what's there, as the file has it.
            if s.list {
                v["file_only"] = json!(file_only(&s.name).unwrap_or("edit it in config.toml"));
                v["entries"] = json!(get(&doc, &s.name).and_then(toml::Value::as_array).map(|a| a.iter().map(|e| e.get("name").and_then(toml::Value::as_str).unwrap_or("?").to_string()).collect::<Vec<_>>()).unwrap_or_default());
            }
            v
        })
        .collect();
    // Free-form sections ([groups]): their keys as the file has them.
    for s in out.iter_mut().filter(|s| s["name"] == "groups") {
        if let Some(t) = doc.get("groups").and_then(toml::Value::as_table) {
            s["fields"] = json!(t.iter().map(|(k, v)| json!({ "path": format!("groups.{k}"), "key": k, "kind": kind(v), "help": "machines in this group (@group, fleet_run)", "set": true, "value": json_of(v), "sample": Value::Null })).collect::<Vec<_>>());
        }
    }
    let unknown = unknown(&doc, &fields, &sections);
    json!({ "sections": out, "unknown": unknown })
}

/// A value from the page, as it goes into config.toml for a field of `kind`.
pub fn item(path: &str, kind: &str, v: &Value) -> Result<toml_edit::Item, String> {
    let name = path;
    Ok(match kind {
        "bool" => toml_edit::value(v.as_bool().or_else(|| v.as_str().map(|s| s.trim() == "true")).ok_or_else(|| format!("{name} is on or off"))?),
        "int" => toml_edit::value(v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).ok_or_else(|| format!("{name} should be a whole number"))?),
        "real" => toml_edit::value(v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).ok_or_else(|| format!("{name} should be a number"))?),
        "list" => {
            let list: Vec<String> = match v {
                Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect(),
                Value::String(t) => t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                _ => return Err(format!("{name} should be a list")),
            };
            crate::config::strings(&list)
        }
        "table" => {
            // An inline table, written as TOML on the page ({ a = 1 }).
            let text = v.as_str().ok_or_else(|| format!("{name} should be written as {{ key = value }}"))?;
            let parsed: toml_edit::DocumentMut = format!("v = {}", text.trim()).parse().map_err(|e| format!("{name}: {e}"))?;
            parsed.get("v").cloned().ok_or_else(|| format!("{name}: no value"))?
        }
        _ => toml_edit::value(v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())),
    })
}

/// A field by its path (the example's, or a free-form section's).
pub fn field(path: &str) -> Option<Field> {
    let (_, fields) = schema();
    if let Some(f) = fields.into_iter().find(|f| f.path == path) {
        return Some(f);
    }
    // [groups] names and the like: lists of names.
    let (section, key) = path.rsplit_once('.')?;
    matches!(section, "groups").then(|| Field { path: path.into(), section: section.into(), key: key.into(), kind: "list", sample: toml::Value::Array(vec![]), help: String::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_in_the_example_is_a_field_with_its_type_and_help() {
        let (sections, fields) = schema();
        let f = |p: &str| fields.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("no field {p}"));
        assert_eq!(f("url").kind, "text");
        assert_eq!(f("planner.max_blocks").kind, "int");
        assert_eq!(f("decide.min_confidence").kind, "real");
        assert_eq!(f("system.deny_paths").kind, "list");
        assert!(f("backup.dir").help.contains("NAS"), "its comment is its help: {:?}", f("backup.dir").help);
        assert!(f("decide.max_state_chars").help.contains("raise with"), "an inline comment with # in it reads: {:?}", f("decide.max_state_chars").help);
        assert!(sections.iter().any(|s| s.name == "capabilities.mcp" && s.list));
        assert!(!fields.iter().any(|f| f.path.starts_with("capabilities.mcp")), "a list's entries aren't fields");
        assert!(fields.len() > 150, "{} fields", fields.len());
        assert!(file_only("system.allow_commands").is_some() && file_only("planner.day_end").is_none());
        assert!(careful("web.entra.tenant").is_some() && secret("embedding.api_key"));
    }

    #[test]
    fn the_page_shows_values_hides_keys_and_lists_what_lyra_ignores() {
        let p = page("url = \"http://x/v1\"\n[embedding]\nurl = \"http://e/v1\"\napi_key = \"sk-secret\"\nmodel = \"e\"\n[pmi]\nurl = \"old\"\n");
        let text = p.to_string();
        assert!(!text.contains("sk-secret"), "a key never goes to the page");
        let emb = p["sections"].as_array().unwrap().iter().find(|s| s["name"] == "embedding").unwrap();
        let key = emb["fields"].as_array().unwrap().iter().find(|f| f["key"] == "api_key").unwrap();
        assert_eq!((key["secret"].clone(), key["has_value"].clone()), (json!(true), json!(true)));
        assert!(p["unknown"].as_array().unwrap().iter().any(|u| u == "pmi.url"));
        assert!(page("[broken").get("error").is_some());
        assert!(item("x", "table", &json!("{ a = 1 }")).is_ok() && item("x", "int", &json!("nope")).is_err());
    }
}
