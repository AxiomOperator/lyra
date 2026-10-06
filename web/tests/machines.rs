//! A machine (`lyra node`) over a real WebSocket: hello, calls answered,
//! timeouts, disconnects, and tokens kept to their own endpoints.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use lyra_web::{Devices, Hub, Inbound, Settings};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

fn pair(dir: &std::path::Path, name: &str, kind: &str) -> String {
    let d = Devices::open(dir).unwrap();
    let code = d.new_code(5).unwrap();
    d.pair(&code, name, kind).unwrap().1
}

#[test]
fn a_machine_answers_calls_and_its_going_away_is_noticed() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = std::env::temp_dir().join(format!("lyra-machines-{}", std::process::id()));
    let (tx, inbound) = std::sync::mpsc::channel();
    let settings = Settings { listen: "127.0.0.1:0".into(), ..Settings::default() };
    let hub = Hub::start(rt.handle(), &settings, &dir, tx).unwrap();
    let node_token = pair(&dir, "desktop", "node");
    let phone_token = pair(&dir, "phone", "device");
    let addr = hub.address;

    // Tokens stay on their own endpoints.
    let refused = rt.block_on(tokio_tungstenite::connect_async(format!("ws://{addr}/node?token={phone_token}")));
    assert!(refused.is_err(), "a phone can't pose as a machine");
    let refused = rt.block_on(tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={node_token}")));
    assert!(refused.is_err(), "a machine can't chat");

    // The fake node: says hello, answers `check` with auto and `call` with an echo;
    // a call for "slow" is never answered.
    let (mut ws, _) = rt.block_on(tokio_tungstenite::connect_async(format!("ws://{addr}/node?token={node_token}"))).unwrap();
    rt.block_on(ws.send(Message::Text(json!({ "type": "hello", "hostname": "pc", "os": "Fedora", "user": "me" }).to_string().into()))).unwrap();
    let node = rt.spawn(async move {
        while let Some(Ok(Message::Text(t))) = ws.next().await {
            let v: Value = serde_json::from_str(t.as_str()).unwrap();
            if v["type"] == "welcome" || v["tool"] == "slow" {
                continue;
            }
            if v["tool"] == "quit" {
                break;
            }
            let reply = json!({ "type": "result", "id": v["id"], "ok": true, "value": { "kind": v["type"], "args": v["args"] } });
            ws.send(Message::Text(reply.to_string().into())).await.unwrap();
        }
    });
    assert!(matches!(inbound.recv_timeout(Duration::from_secs(5)), Ok(Inbound::MachinesChanged)));
    let machines = hub.machines();
    assert_eq!((machines.len(), machines[0].name.as_str(), machines[0].os.as_str()), (1, "desktop", "Fedora"));

    let r = hub.call_machine("Desktop", json!({ "type": "call", "tool": "shell_run", "args": { "command": "ls" } }), Duration::from_secs(5)).unwrap();
    assert_eq!(r, json!({ "kind": "call", "args": { "command": "ls" } }));
    let slow = hub.call_machine("desktop", json!({ "type": "call", "tool": "slow" }), Duration::from_millis(300));
    assert!(slow.unwrap_err().contains("didn't answer"));
    assert!(hub.call_machine("laptop", json!({}), Duration::from_secs(1)).unwrap_err().contains("isn't connected"));

    // The node goes away: lyra hears about it and the machine is gone.
    let _ = hub.call_machine("desktop", json!({ "type": "call", "tool": "quit" }), Duration::from_millis(500));
    rt.block_on(node).unwrap();
    assert!(matches!(inbound.recv_timeout(Duration::from_secs(5)), Ok(Inbound::MachinesChanged)));
    assert!(hub.machines().is_empty());
    assert!(hub.call_machine("desktop", json!({}), Duration::from_secs(1)).unwrap_err().contains("isn't connected"));
}

#[test]
fn a_headless_machine_pairs_when_a_device_approves() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = std::env::temp_dir().join(format!("lyra-headless-{}", std::process::id()));
    let (tx, inbound) = std::sync::mpsc::channel();
    let node_bin = dir.join("lyra-node-bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&node_bin, b"pretend program").unwrap();
    let settings = Settings { listen: "127.0.0.1:0".into(), node_binary: node_bin.display().to_string(), public_url: "https://lyra.example.com".into(), ..Settings::default() };
    let hub = Hub::start(rt.handle(), &settings, &dir, tx).unwrap();
    let base = format!("http://{}", hub.address);
    let client = reqwest::blocking::Client::new();
    let post = |path: &str, body: Value| client.post(format!("{base}{path}")).json(&body).send().unwrap().json::<Value>().unwrap();
    let get = |path: &str| client.get(format!("{base}{path}")).send().unwrap();

    let asked = post("/api/pair/request", json!({ "name": "web1", "hostname": "web1.lan", "os": "Debian" }));
    let (id, code) = (asked["id"].as_str().unwrap().to_string(), asked["code"].as_str().unwrap().to_string());
    assert_eq!(code.len(), 4);
    assert!(matches!(inbound.recv_timeout(Duration::from_secs(5)), Ok(Inbound::PairRequested(p)) if p.name == "web1" && p.kind == "node"));
    assert_eq!(hub.pair_requests().len(), 1);
    assert_eq!(get(&format!("/api/pair/request/{id}")).json::<Value>().unwrap()["state"], "waiting");

    assert!(hub.answer_pair("ZZZZ", true).is_err(), "the code must match");
    assert!(hub.answer_pair(&code.to_lowercase(), true).unwrap().contains("paired web1"));
    let done = get(&format!("/api/pair/request/{id}")).json::<Value>().unwrap();
    assert_eq!(done["state"], "approved");
    let token = done["token"].as_str().unwrap();
    assert!(hub.devices().authenticate(token).is_some_and(|d| d.kind == "node" && d.name == "web1"));
    assert_eq!(get(&format!("/api/pair/request/{id}")).json::<Value>().unwrap()["state"], "unknown", "the token is handed over once");

    // Denied, and too many at once.
    let denied = post("/api/pair/request", json!({ "name": "x" }));
    hub.answer_pair(denied["code"].as_str().unwrap(), false).unwrap();
    assert_eq!(get(&format!("/api/pair/request/{}", denied["id"].as_str().unwrap())).json::<Value>().unwrap()["state"], "denied");
    for i in 0..5 {
        post("/api/pair/request", json!({ "name": format!("m{i}") }));
    }
    assert!(post("/api/pair/request", json!({ "name": "one-too-many" }))["error"].as_str().unwrap().contains("too many"));
    assert!(post("/api/pair/request", json!({ "name": "" }))["error"].is_string(), "a name is needed");

    // Handing out lyra-node, its checksum, and the installer pointing here.
    assert_eq!(get("/download/lyra-node").bytes().unwrap().as_ref(), b"pretend program");
    let sum = get("/download/lyra-node.sha256").text().unwrap();
    assert_eq!(sum.split_whitespace().next().unwrap(), hub.node_build().unwrap());
    let script = get("/install.sh").text().unwrap();
    assert!(script.contains("URL=\"https://lyra.example.com\"") && script.contains("lyra-node\" pair"));
    // The app is stamped with its version.
    let page = get("/").text().unwrap();
    assert!(page.contains(&format!("content=\"{}\"", hub.app_version())), "the page carries the app's version");
    assert!(get("/sw.js").text().unwrap().contains(hub.app_version()));
    assert_eq!(get("/manifest.webmanifest").status(), 200);
    assert_eq!(get("/nope.js").status(), 404);
}

#[test]
fn devices_only_see_their_own_conversation() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = std::env::temp_dir().join(format!("lyra-sessions-hub-{}", std::process::id()));
    let (tx, inbound) = std::sync::mpsc::channel();
    let settings = Settings { listen: "127.0.0.1:0".into(), ..Settings::default() };
    let hub = Hub::start(rt.handle(), &settings, &dir, tx).unwrap();
    let token = pair(&dir, "phone", "device");
    // A pretend app loop: snapshots name their conversation ("" = "main").
    let (conns_tx, conns_rx) = std::sync::mpsc::channel::<u64>();
    std::thread::spawn(move || {
        while let Ok(msg) = inbound.recv() {
            match msg {
                Inbound::Snapshot { session, reply } => {
                    let s = if session.is_empty() { "main".to_string() } else { session };
                    let _ = reply.send(json!({ "type": "snapshot", "session_id": s }));
                }
                Inbound::Send { conn, .. } => {
                    let _ = conns_tx.send(conn);
                }
                _ => {}
            }
        }
    });
    let addr = hub.address;
    let open = |session: &str| {
        let (ws, _) = rt.block_on(tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={token}&session={session}"))).unwrap();
        ws
    };
    let next = |ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>| -> Value {
        let m = rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), ws.next()).await }).unwrap().unwrap().unwrap();
        serde_json::from_str(m.to_text().unwrap()).unwrap()
    };
    let mut a = open("");
    let mut b = open("other");
    assert_eq!(next(&mut a)["session_id"], "main");
    assert_eq!(next(&mut b)["session_id"], "other");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(hub.attached_sessions(), vec!["main".to_string(), "other".to_string()]);

    hub.publish(Some("other"), json!({ "type": "add", "text": "for other" }));
    hub.publish(Some("main"), json!({ "type": "add", "text": "for main" }));
    hub.publish(None, json!({ "type": "status", "text": "for everyone" }));
    assert_eq!(next(&mut a)["text"], "for main");
    assert_eq!(next(&mut a)["text"], "for everyone");
    assert_eq!(next(&mut b)["text"], "for other");
    assert_eq!(next(&mut b)["text"], "for everyone");

    // `a` asks for something; the app moves it to another conversation.
    rt.block_on(a.send(Message::Text(json!({ "type": "send", "text": "/new" }).to_string().into()))).unwrap();
    let conn = conns_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    hub.attach(conn, "fresh");
    assert_eq!(next(&mut a)["session_id"], "fresh", "a new snapshot for the new conversation");
    hub.publish(Some("main"), json!({ "type": "add", "text": "old" }));
    hub.publish(Some("fresh"), json!({ "type": "add", "text": "new" }));
    assert_eq!(next(&mut a)["text"], "new", "the old conversation's updates no longer arrive");
    let d = hub.devices().list().into_iter().find(|d| d.name == "phone").unwrap();
    assert!(matches!(d.last_session.as_deref(), Some("fresh") | Some("other")), "the device remembers where it was");
}
