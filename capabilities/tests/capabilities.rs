//! Capability intelligence end to end: policy (C4), discovery and scoring
//! (C3, C6, C9), usage and health (C5, C8), and the OpenAPI and MCP providers
//! with their requirements and verification rules (C7, C9, C11).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use lyra_capabilities::index::CapabilityIndex;
use lyra_capabilities::mcp::{McpClient, McpConfig};
use lyra_capabilities::openapi::{OpenApiClient, OpenApiConfig};
use lyra_capabilities::store::UsageStore;
use lyra_capabilities::{
    Capability, CapabilityHealth, CapabilityKind, CapabilityManager, CapabilityUsage, Policy, RiskLevel, Rule, Settings,
};
use lyra_memory::embedding::FakeEmbedder;
use serde_json::json;
use uuid::Uuid;

async fn manager(settings: Settings) -> CapabilityManager {
    let dir = std::env::temp_dir().join(format!("lyra-caps-{}", Uuid::new_v4()));
    CapabilityManager::with(UsageStore::in_memory().await.unwrap(), CapabilityIndex::open(&dir).await.unwrap(), settings).await.unwrap()
}

fn cap(id: &str, description: &str, risk: RiskLevel) -> Capability {
    Capability::new(id, CapabilityKind::NativeTool, description, risk)
}

fn usage(id: &str, success: bool, ms: u64) -> CapabilityUsage {
    CapabilityUsage { capability_id: id.into(), run_id: None, success, duration_ms: ms, retries: 0, error_code: None, error: None }
}

#[test]
fn policy_is_decided_by_risk_and_overrides() {
    let mut p = Policy::default();
    assert_eq!(p.decide(&cap("a", "", RiskLevel::ReadOnly)), Rule::Auto);
    assert_eq!(p.decide(&cap("a", "", RiskLevel::Destructive)), Rule::Approval);
    assert_eq!(p.decide(&cap("a", "", RiskLevel::Privileged)), Rule::Deny);
    let mut asks = cap("a", "", RiskLevel::Write);
    asks.metadata.requires_approval = true;
    assert_eq!(p.decide(&asks), Rule::Approval, "a capability asking for approval gets it");
    p.overrides.insert("native.*".into(), Rule::Deny);
    assert_eq!(p.decide(&cap("x", "", RiskLevel::ReadOnly)), Rule::Deny, "overrides by source");
    let mut off = cap("y", "", RiskLevel::ReadOnly);
    off.enabled = false;
    assert_eq!(Policy::default().decide(&off), Rule::Deny);
}

#[tokio::test]
async fn discovery_finds_few_relevant_capabilities_and_track_record_matters() {
    let m = manager(Settings::default()).await;
    m.set_capabilities(vec![
        cap("tasks.create", "Create a task in a project", RiskLevel::Write),
        cap("tasks.create_quick", "Create a task quickly in a project", RiskLevel::Write),
        cap("projects.search", "Search projects by name", RiskLevel::ReadOnly),
        cap("mail.send", "Send an email", RiskLevel::Write),
        cap("files.delete", "Delete a file", RiskLevel::Destructive),
        cap("admin.reset", "Reset everything", RiskLevel::Privileged),
    ])
    .await
    .unwrap();
    let found = m.discover("create a task for the website project", 3, &[]).await.unwrap();
    let ids: Vec<&str> = found.iter().map(|f| f.capability.id.as_str()).collect();
    assert!(ids.contains(&"tasks.create") && !ids.contains(&"mail.send"), "{ids:?}");
    assert!(m.discover("reset everything", 5, &[]).await.unwrap().is_empty(), "denied capabilities aren't offered");

    // History: the quick one keeps failing, the other works — prefer the one that works.
    for _ in 0..6 {
        m.record(usage("tasks.create_quick", false, 4000)).await.unwrap();
        m.record(usage("tasks.create", true, 120)).await.unwrap();
    }
    let found = m.discover("create a task quickly", 2, &[]).await.unwrap();
    assert_eq!(found[0].capability.id, "tasks.create", "{:?}", found.iter().map(|f| (&f.capability.id, f.score)).collect::<Vec<_>>());
    let quick = m.get("tasks.create_quick").unwrap();
    assert_eq!(m.health(&quick), CapabilityHealth::Degraded, "recent failures degrade it");
    assert_eq!(quick.metadata.success_rate, Some(0.0));
    assert_eq!(m.get("tasks.create").unwrap().metadata.average_latency_ms, Some(120));
}

#[tokio::test]
async fn health_and_session_approvals() {
    let m = manager(Settings::default()).await;
    let mut remote = cap("remote.read", "Read the remote thing", RiskLevel::ReadOnly);
    remote.source = "remote".into();
    m.set_capabilities(vec![remote.clone(), cap("files.delete", "Delete a file", RiskLevel::Destructive)]).await.unwrap();
    m.set_health("remote.*", CapabilityHealth::Unavailable);
    assert!(m.usable().iter().all(|c| c.id != "remote.read"), "unavailable capabilities are left out");
    let delete = m.get("files.delete").unwrap();
    assert_eq!(m.rule(&delete), Rule::Approval);
    m.allow("files.delete").unwrap();
    assert_eq!(m.rule(&delete), Rule::Auto, "allowed for the session");
    let mut s = Settings::default();
    s.policy.privileged = Rule::Deny;
    let m2 = manager(s).await;
    m2.set_capabilities(vec![cap("admin.reset", "Reset", RiskLevel::Privileged)]).await.unwrap();
    assert!(m2.allow("admin.reset").is_err(), "denied can't be allowed");
}

#[tokio::test]
async fn semantic_discovery_finds_what_keywords_miss() {
    let m = manager(Settings::default()).await;
    let task = cap("tasks.create", "Create a task in a project", RiskLevel::Write);
    let mail = cap("mail.send", "Send an email", RiskLevel::Write);
    let e = FakeEmbedder::new("emb", 4)
        .with(&task.search_text(), &[1.0, 0.0, 0.0, 0.0])
        .with(&mail.search_text(), &[0.0, 1.0, 0.0, 0.0])
        .with("open a ticket for the bug", &[0.95, 0.05, 0.0, 0.0]);
    m.set_embedder(Some(Arc::new(e)));
    let notes = m.set_capabilities(vec![task, mail]).await.unwrap();
    assert!(notes.iter().any(|n| n.contains("indexed 2")), "{notes:?}");
    let found = m.discover("open a ticket for the bug", 1, &[]).await.unwrap();
    assert_eq!(found[0].capability.id, "tasks.create", "no shared words, same meaning");
    // Unchanged capabilities aren't embedded again.
    let again = m.set_capabilities(m.all()).await.unwrap();
    assert!(!again.iter().any(|n| n.contains("indexed")), "{again:?}");
}

const SPEC: &str = r#"
openapi: 3.0.0
servers: [{ url: "http://127.0.0.1:1" }]
paths:
  /projects:
    get: { operationId: projects.list, summary: List projects, parameters: [{ name: q, in: query, schema: { type: string } }] }
  /projects/{projectId}/tasks:
    post:
      operationId: tasks.create
      summary: Create a task
      parameters: [{ name: projectId, in: path, required: true, schema: { type: string } }]
      requestBody: { required: true, content: { application/json: { schema: { type: object, properties: { title: { type: string } } } } } }
  /projects/{projectId}/tasks/{taskId}:
    get:
      operationId: tasks.get
      summary: Get a task
      parameters:
        - { name: projectId, in: path, required: true, schema: { type: string } }
        - { name: taskId, in: path, required: true, schema: { type: string } }
    delete:
      operationId: tasks.delete
      summary: Delete a task
      parameters:
        - { name: projectId, in: path, required: true, schema: { type: string } }
        - { name: taskId, in: path, required: true, schema: { type: string } }
"#;

fn config(base_url: Option<String>) -> OpenApiConfig {
    OpenApiConfig {
        name: "pm".into(),
        spec: "pm.yaml".into(),
        base_url,
        auth_env: Some("LYRA_TEST_PM_TOKEN".into()),
        auth_header: "Authorization".into(),
        auth_scheme: "Bearer".into(),
    }
}

#[test]
fn openapi_operations_become_capabilities_with_requirements_and_verification() {
    let client = OpenApiClient::load(config(None), SPEC).unwrap();
    let caps = client.capabilities();
    let by = |id: &str| caps.iter().find(|c| c.id == id).unwrap().clone();
    assert_eq!(by("pm.projects.list").risk, RiskLevel::ReadOnly);
    assert_eq!(by("pm.tasks.delete").risk, RiskLevel::Destructive);
    let create = by("pm.tasks.create");
    assert_eq!((create.risk, create.kind, create.name.as_str()), (RiskLevel::Write, CapabilityKind::OpenApi, "pm_tasks_create"));
    assert_eq!(create.input_schema["required"], json!(["projectId", "body"]));
    let req = &create.metadata.requirements[0];
    assert_eq!((req.entity.as_str(), req.resolution_capability.as_deref()), ("projectId", Some("pm.projects.list")));
    let v = create.metadata.verification.unwrap();
    assert_eq!(v.capability, "pm.tasks.get");
    assert_eq!(v.arguments, json!({ "projectId": "{{args.projectId}}", "taskId": "{{result.id}}" }));
}

#[test]
fn openapi_calls_fill_the_path_query_body_and_credential() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        let mut len = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
            if line == "\r\n" {
                break;
            }
            head += &line;
        }
        let mut body = vec![0; len];
        reader.read_exact(&mut body).unwrap();
        let reply = r#"{"id":"t-7","title":"Fix"}"#;
        write!(stream, "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}", reply.len()).unwrap();
        (head, String::from_utf8(body).unwrap())
    });
    // SAFETY: the test sets a variable only it reads.
    unsafe { std::env::set_var("LYRA_TEST_PM_TOKEN", "s3cret") };
    let client = OpenApiClient::load(config(Some(format!("http://127.0.0.1:{port}"))), SPEC).unwrap();
    let out = client.call("pm.tasks.create", &json!({ "projectId": "web site", "body": { "title": "Fix" } })).unwrap();
    assert_eq!(out["id"], "t-7");
    let (head, body) = server.join().unwrap();
    assert!(head.starts_with("POST /projects/web%20site/tasks "), "{head}");
    assert!(head.to_lowercase().contains("authorization: bearer s3cret"));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap(), json!({ "title": "Fix" }));
    assert!(client.call("pm.tasks.create", &json!({ "body": {} })).unwrap_err().contains("projectId"));
}

/// A tiny MCP server (stdio, JSON-RPC) written in Python, when Python is around.
const MCP_SERVER: &str = r#"
import sys, json
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    m, i = msg["method"], msg["id"]
    if m == "initialize":
        r = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "test", "version": "1"}}
    elif m == "tools/list":
        r = {"tools": [
            {"name": "echo", "description": "Echo text back", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}, "annotations": {"readOnlyHint": True}},
            {"name": "wipe", "description": "Wipe everything", "inputSchema": {"type": "object"}, "annotations": {"destructiveHint": True}},
        ]}
    elif m == "tools/call":
        a = msg["params"]["arguments"]
        if msg["params"]["name"] == "echo":
            r = {"content": [{"type": "text", "text": a.get("text", "")}]}
        else:
            r = {"content": [{"type": "text", "text": "refused"}], "isError": True}
    elif m == "ping":
        r = {}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": i, "error": {"code": -32601, "message": "nope"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": i, "result": r}), flush=True)
"#;

#[test]
fn mcp_tools_become_capabilities_and_can_be_called() {
    if std::process::Command::new("python3").arg("--version").output().is_err() {
        eprintln!("python3 not found; skipping");
        return;
    }
    let path = std::env::temp_dir().join(format!("lyra-mcp-{}.py", Uuid::new_v4()));
    std::fs::write(&path, MCP_SERVER).unwrap();
    let client = McpClient::start(McpConfig {
        name: "t".into(),
        command: "python3".into(),
        args: vec![path.to_string_lossy().into()],
        env: Default::default(),
        cwd: None,
    })
    .unwrap();
    let caps = client.capabilities();
    let echo = caps.iter().find(|c| c.id == "t.echo").unwrap();
    assert_eq!((echo.kind, echo.risk), (CapabilityKind::Mcp, RiskLevel::ReadOnly));
    assert_eq!(caps.iter().find(|c| c.id == "t.wipe").unwrap().risk, RiskLevel::Destructive);
    assert_eq!(client.call("t.echo", &json!({ "text": "hello" })).unwrap(), json!({ "content": "hello" }));
    assert_eq!(client.call("t.wipe", &json!({})).unwrap_err(), "refused");
    assert_eq!(client.health(), CapabilityHealth::Healthy);
    drop(client);
    let _ = std::fs::remove_file(path);
}
