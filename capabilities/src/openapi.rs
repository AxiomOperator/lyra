//! OpenAPI operations as capabilities: each operation in a spec (JSON or
//! YAML) becomes one, with its parameters as the input schema, a risk level
//! from its method (or `x-lyra-risk`), the identifiers it needs and how to
//! find them (C7), and a read-back verification for writes (C9).
//! Credentials are never stored here: they're read from an environment
//! variable, or handed over by the caller (lyra's secrets file). Operations can
//! be filtered by tag, path and method, and fixed parameters (an organization's
//! id) filled in so the model never has to supply them.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::model::{Capability, CapabilityHealth, CapabilityKind, CapabilityRequirement, RiskLevel, VerificationRule};

/// `[[capabilities.openapi]]`.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenApiConfig {
    /// Prefix for its capabilities, e.g. `pmi` → `pmi.tasks.create`.
    pub name: String,
    /// Path to the spec (`~/` is expanded by the caller).
    pub spec: String,
    /// Overrides the spec's first server URL.
    pub base_url: Option<String>,
    /// Environment variable holding the credential (never the credential itself).
    pub auth_env: Option<String>,
    #[serde(default = "authorization")]
    pub auth_header: String,
    /// Prefix for the credential, e.g. `Bearer`; empty for none.
    #[serde(default = "bearer")]
    pub auth_scheme: String,
    /// A token from lyra's secrets file (`[name] token`), read by the caller.
    #[serde(default)]
    pub auth_secret: Option<String>,
    /// The credential itself, filled in by the caller from `auth_secret`; never in the config.
    #[serde(skip)]
    pub secret: Option<String>,
    /// Only operations with one of these tags (empty: all).
    #[serde(default)]
    pub include_tags: Vec<String>,
    #[serde(default)]
    pub exclude_tags: Vec<String>,
    /// Paths left out, `*` matching anything ("/v1/admin/*").
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Only these methods (empty: get, post, put, patch, delete).
    #[serde(default)]
    pub methods: Vec<String>,
    /// Parameters always given these values (and not asked of the model), e.g. `orgId`.
    #[serde(default)]
    pub defaults: std::collections::HashMap<String, String>,
    /// Operations that change things ask the user first (default true).
    #[serde(default = "yes")]
    pub approve_writes: bool,
}

fn yes() -> bool {
    true
}

impl Default for OpenApiConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            spec: String::new(),
            base_url: None,
            auth_env: None,
            auth_header: authorization(),
            auth_scheme: bearer(),
            auth_secret: None,
            secret: None,
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
            exclude: Vec::new(),
            methods: Vec::new(),
            defaults: Default::default(),
            approve_writes: true,
        }
    }
}

/// `*` matches any run of characters.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            let Some(r) = rest.strip_prefix(part) else { return false };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            let Some(at) = rest.find(part) else { return false };
            rest = &rest[at + part.len()..];
        }
    }
    true
}

fn authorization() -> String {
    "Authorization".into()
}

fn bearer() -> String {
    "Bearer".into()
}

#[derive(Debug, Clone)]
struct Param {
    name: String,
    location: String,
    required: bool,
}

#[derive(Debug, Clone)]
struct Operation {
    id: String,
    method: String,
    path: String,
    params: Vec<Param>,
    has_body: bool,
    capability: Capability,
}

pub struct OpenApiClient {
    pub config: OpenApiConfig,
    base_url: String,
    operations: Vec<Operation>,
}

/// Follow a local `$ref` (`#/components/...`) once.
fn resolve<'a>(spec: &'a Value, v: &'a Value) -> &'a Value {
    match v.get("$ref").and_then(Value::as_str).and_then(|r| r.strip_prefix("#/")) {
        Some(path) => path.split('/').fold(spec, |node, key| &node[key.replace("~1", "/").replace("~0", "~")]),
        None => v,
    }
}

/// A schema with every local `$ref` inside it replaced by what it points to
/// (to a depth, so a recursive schema ends as a plain object).
fn inline(spec: &Value, v: &Value, depth: usize) -> Value {
    if depth > 8 {
        return json!({ "type": "object" });
    }
    match v {
        Value::Object(map) if map.contains_key("$ref") => {
            let target = resolve(spec, v);
            if target.is_null() { json!({}) } else { inline(spec, target, depth + 1) }
        }
        Value::Object(map) => Value::Object(map.iter().map(|(k, x)| (k.clone(), inline(spec, x, depth))).collect()),
        Value::Array(items) => Value::Array(items.iter().map(|x| inline(spec, x, depth)).collect()),
        other => other.clone(),
    }
}

/// `tasks.create` from an operationId like `createTask`/`tasks_create`, or from method and path.
fn operation_id(op: &Value, method: &str, path: &str) -> String {
    let raw = match op.get("operationId").and_then(Value::as_str) {
        Some(id) => id.to_string(),
        None => {
            // "/v1/orgs/{orgId}/tasks/{taskId}" → "orgs.tasks.item.get" (a version
            // segment dropped, a trailing parameter kept apart from the list).
            let all: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            let mut segments: Vec<&str> = all.iter().copied().filter(|s| !s.starts_with('{')).filter(|s| !(s.len() >= 2 && s.starts_with('v') && s[1..].chars().all(|c| c.is_ascii_digit()))).collect();
            if all.last().is_some_and(|s| s.starts_with('{')) {
                segments.push("item");
            }
            format!("{}.{method}", segments.join("."))
        }
    };
    raw.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' { c } else { '_' }).collect()
}

fn risk(op: &Value, method: &str) -> RiskLevel {
    if let Some(r) = op.get("x-lyra-risk").and_then(Value::as_str)
        && let Ok(r) = serde_json::from_value::<RiskLevel>(json!(r))
    {
        return r;
    }
    match method {
        "get" | "head" | "options" => RiskLevel::ReadOnly,
        "delete" => RiskLevel::Destructive,
        _ => RiskLevel::Write,
    }
}

/// `projects` for `projectId` / `project_id`.
fn collection(param: &str) -> String {
    let base = param.trim_end_matches("Id").trim_end_matches("_id").trim_end_matches("ID").to_lowercase();
    if base.ends_with('s') { base } else { format!("{base}s") }
}

impl OpenApiClient {
    pub fn load(config: OpenApiConfig, spec_text: &str) -> Result<Self, String> {
        let spec: Value = if spec_text.trim_start().starts_with('{') {
            serde_json::from_str(spec_text).map_err(|e| format!("{}: {e}", config.spec))?
        } else {
            serde_yaml_ng::from_str(spec_text).map_err(|e| format!("{}: {e}", config.spec))?
        };
        let base_url = config
            .base_url
            .clone()
            .or_else(|| spec["servers"][0]["url"].as_str().map(str::to_string))
            .ok_or_else(|| format!("{}: no servers in the spec; set base_url", config.name))?;
        let mut operations = Vec::new();
        let methods: Vec<String> = if config.methods.is_empty() { ["get", "post", "put", "patch", "delete"].map(String::from).to_vec() } else { config.methods.iter().map(|m| m.to_lowercase()).collect() };
        for (path, item) in spec["paths"].as_object().into_iter().flatten() {
            if config.exclude.iter().any(|g| glob(g, path)) {
                continue;
            }
            let shared: Vec<Value> = item["parameters"].as_array().cloned().unwrap_or_default();
            for method in ["get", "post", "put", "patch", "delete"] {
                let Some(op) = item.get(method) else { continue };
                if !methods.iter().any(|m| m == method) {
                    continue;
                }
                let tags: Vec<String> = op["tags"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(str::to_string)).collect();
                if (!config.include_tags.is_empty() && !tags.iter().any(|t| config.include_tags.contains(t))) || tags.iter().any(|t| config.exclude_tags.contains(t)) {
                    continue;
                }
                let mut id = format!("{}.{}", config.name, operation_id(op, method, path));
                // Two operations named alike: the later one says where it goes.
                if operations.iter().any(|o: &Operation| o.id == id) {
                    id = format!("{id}_{}", operations.len());
                }
                let mut properties = Map::new();
                let mut required = Vec::new();
                let mut params = Vec::new();
                for p in shared.iter().chain(op["parameters"].as_array().into_iter().flatten()) {
                    let p = resolve(&spec, p);
                    let (Some(name), Some(location)) = (p["name"].as_str(), p["in"].as_str()) else { continue };
                    if location == "cookie" {
                        continue;
                    }
                    let is_required = p["required"].as_bool().unwrap_or(location == "path");
                    params.push(Param { name: name.into(), location: location.into(), required: is_required });
                    // Filled in by lyra: not the model's to give.
                    if config.defaults.contains_key(name) {
                        continue;
                    }
                    let mut schema = inline(&spec, &p["schema"], 0);
                    if let (Some(d), Some(obj)) = (p["description"].as_str(), schema.as_object_mut()) {
                        obj.insert("description".into(), json!(d));
                    }
                    if is_required {
                        required.push(json!(name));
                    }
                    properties.insert(name.to_string(), if schema.is_null() { json!({ "type": "string" }) } else { schema });
                }
                let body = resolve(&spec, &op["requestBody"]);
                let has_body = !body.is_null();
                if has_body {
                    let schema = inline(&spec, &body["content"]["application/json"]["schema"], 0);
                    properties.insert("body".into(), if schema.is_null() { json!({ "type": "object" }) } else { schema });
                    if body["required"].as_bool().unwrap_or(false) {
                        required.push(json!("body"));
                    }
                }
                let description = op["summary"].as_str().or(op["description"].as_str()).unwrap_or(path).trim().to_string();
                let mut cap = Capability::new(&id, CapabilityKind::OpenApi, &format!("{description} ({} {path})", method.to_uppercase()), risk(op, method));
                cap.input_schema = json!({ "type": "object", "properties": properties, "required": required });
                let ok = ["200", "201"].into_iter().map(|code| &op["responses"][code]["content"]["application/json"]["schema"]).find(|s| s.is_object());
                cap.output_schema = ok.map(|schema| inline(&spec, schema, 0));
                cap.source = config.name.clone();
                cap.tags = tags;
                cap.metadata.requires_approval = config.approve_writes && cap.risk != RiskLevel::ReadOnly;
                cap.permissions = vec![format!("{}.{}", config.name, if cap.risk == RiskLevel::ReadOnly { "read" } else { "write" })];
                cap.metadata.idempotent = matches!(method, "get" | "put" | "delete" | "head");
                operations.push(Operation { id, method: method.into(), path: path.clone(), params, has_body, capability: cap });
            }
        }
        let mut client = Self { config, base_url: base_url.trim_end_matches('/').to_string(), operations };
        client.link();
        Ok(client)
    }

    /// Requirements (C7) and verifications (C9) between operations.
    fn link(&mut self) {
        let ops = self.operations.clone();
        let find_get = |path: &str| ops.iter().find(|o| o.method == "get" && o.path == path).map(|o| o.id.clone());
        for op in &mut self.operations {
            // Path identifiers, and what lists them.
            for p in op.params.iter().filter(|p| p.location == "path") {
                let coll = collection(&p.name);
                let resolver = ops
                    .iter()
                    .find(|o| o.method == "get" && o.path.trim_end_matches('/').ends_with(&format!("/{coll}")))
                    .or_else(|| ops.iter().find(|o| o.method == "get" && o.id.to_lowercase().contains("search")))
                    .map(|o| o.id.clone());
                op.capability.metadata.requirements.push(CapabilityRequirement { entity: p.name.clone(), resolution_capability: resolver });
            }
            // Writes are checked by reading the thing back.
            let item_get = |path: &str| {
                ops.iter().find(|o| o.method == "get" && o.path.starts_with(path) && o.path[path.len()..].trim_matches('/').starts_with('{') && !o.path[path.len()..].trim_matches('/').contains('/'))
            };
            let rule = match op.method.as_str() {
                "post" => item_get(op.path.trim_end_matches('/')).map(|get| {
                    let param = get.params.iter().find(|p| p.location == "path" && !op.params.iter().any(|q| q.name == p.name));
                    let mut args = Map::new();
                    for p in get.params.iter().filter(|p| p.location == "path") {
                        let value = if param.is_some_and(|q| q.name == p.name) { "{{result.id}}".to_string() } else { format!("{{{{args.{}}}}}", p.name) };
                        args.insert(p.name.clone(), json!(value));
                    }
                    VerificationRule { capability: get.id.clone(), arguments: Value::Object(args), success_expression: "exists:id".into() }
                }),
                "put" | "patch" => find_get(&op.path).map(|get| {
                    let args: Map<String, Value> =
                        op.params.iter().filter(|p| p.location == "path").map(|p| (p.name.clone(), json!(format!("{{{{args.{}}}}}", p.name)))).collect();
                    VerificationRule { capability: get, arguments: Value::Object(args), success_expression: String::new() }
                }),
                _ => None,
            };
            op.capability.metadata.verification = rule;
        }
    }

    pub fn capabilities(&self) -> Vec<Capability> {
        self.operations.iter().map(|o| o.capability.clone()).collect()
    }

    pub fn has(&self, id: &str) -> bool {
        self.operations.iter().any(|o| o.id == id)
    }

    /// Call an operation. Blocking: run it on a plain thread.
    pub fn call(&self, id: &str, args: &Value) -> Result<Value, String> {
        let op = self.operations.iter().find(|o| o.id == id).ok_or_else(|| format!("unknown operation {id}"))?;
        let mut path = op.path.clone();
        let mut query: Vec<(String, String)> = Vec::new();
        let mut headers: Vec<(String, String)> = Vec::new();
        let text = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_string);
        for p in &op.params {
            let given = self.config.defaults.get(&p.name).map(|v| json!(v));
            match given.as_ref().or(args.get(&p.name)) {
                Some(v) if !v.is_null() => match p.location.as_str() {
                    "path" => path = path.replace(&format!("{{{}}}", p.name), &encode(&text(v))),
                    "query" => query.push((p.name.clone(), text(v))),
                    "header" => headers.push((p.name.clone(), text(v))),
                    _ => {}
                },
                _ if p.required => return Err(format!("missing required parameter {}", p.name)),
                _ => {}
            }
        }
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?;
        let method = reqwest::Method::from_bytes(op.method.to_uppercase().as_bytes()).map_err(|e| e.to_string())?;
        let qs: Vec<String> = query.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect();
        let url = if qs.is_empty() { format!("{}{path}", self.base_url) } else { format!("{}{path}?{}", self.base_url, qs.join("&")) };
        let mut req = client.request(method, url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        if let Some(value) = self.credential()? {
            req = req.header(&self.config.auth_header, value);
        }
        if op.has_body {
            req = req.json(args.get("body").unwrap_or(&json!({})));
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        let value: Value = serde_json::from_str(&body).unwrap_or(Value::String(body));
        if status.is_success() {
            Ok(value)
        } else {
            Err(format!("{status}: {}", value.to_string().chars().take(500).collect::<String>()))
        }
    }

    /// Where its calls go.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The auth header's value: the handed-over secret, else the environment variable.
    fn credential(&self) -> Result<Option<String>, String> {
        let secret = match (&self.config.secret, &self.config.auth_env, &self.config.auth_secret) {
            (Some(s), _, _) => s.clone(),
            (None, Some(var), _) => std::env::var(var).map_err(|_| format!("environment variable {var} isn't set"))?,
            (None, None, Some(name)) => return Err(format!("no token for {name} yet (secrets.toml)")),
            (None, None, None) => return Ok(None),
        };
        Ok(Some(if self.config.auth_scheme.is_empty() { secret } else { format!("{} {secret}", self.config.auth_scheme) }))
    }

    /// Reachable, and taking the credential? Any answer but 401/403 counts.
    pub fn health(&self) -> CapabilityHealth {
        let Ok(client) = reqwest::blocking::Client::builder().timeout(Duration::from_secs(5)).build() else { return CapabilityHealth::Unavailable };
        let mut req = client.get(&self.base_url);
        match self.credential() {
            Ok(Some(value)) => req = req.header(&self.config.auth_header, value),
            Ok(None) => {}
            Err(_) => return CapabilityHealth::Unavailable,
        }
        match req.send() {
            Ok(r) if matches!(r.status().as_u16(), 401 | 403) => CapabilityHealth::Unavailable,
            Ok(_) => CapabilityHealth::Healthy,
            Err(_) => CapabilityHealth::Unavailable,
        }
    }
}

/// Percent-encode a path segment.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}
