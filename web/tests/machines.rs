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
