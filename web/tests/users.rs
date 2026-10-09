//! Users: each device acts as its user, members are kept to their own, and
//! a disabled or pending account can't get in.

use std::time::Duration;

use futures_util::StreamExt;
use lyra_web::{Devices, Hub, Inbound, Role, Settings, Status, User, Users};
use serde_json::{Value, json};

#[test]
fn devices_act_as_their_user() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = std::env::temp_dir().join(format!("lyra-users-hub-{}", std::process::id()));
    // A device paired before users existed: the owner's once the hub starts.
    let devices = Devices::open(&dir).unwrap();
    let old = devices.pair(&devices.new_code(5).unwrap(), "old phone", "device").unwrap().1;
    let (tx, inbound) = std::sync::mpsc::channel();
    let settings = Settings { listen: "127.0.0.1:0".into(), ..Settings::default() };
    let hub = Hub::start(rt.handle(), &settings, &dir, tx).unwrap();
    let dana = User { id: "oid-dana".into(), name: "Dana".into(), email: "dana@fbcad.org".into(), tenant: "t".into(), oid: "oid-dana".into(), role: Role::Member, status: Status::Active, created: chrono::Utc::now(), last_seen: None, tool_rounds: None, username: String::new(), password: String::new(), must_change: false, reset: String::new(), reset_until: None };
    Users::open(&dir).upsert(dana).unwrap();
    let member = devices.pair(&devices.new_code_for(5, Some("oid-dana")).unwrap(), "Dana's phone", "device").unwrap().1;
    // A pretend app loop: says who each snapshot was for.
    std::thread::spawn(move || {
        while let Ok(msg) = inbound.recv() {
            if let Inbound::Snapshot { who, reply, .. } = msg {
                let _ = reply.send(json!({ "type": "snapshot", "session_id": format!("{}-main", who.user), "admin": who.admin }));
            }
        }
    });
    let addr = hub.address;
    let first = |token: &str| -> Value {
        let (mut ws, _) = rt.block_on(tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={token}"))).unwrap();
        let m = rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), ws.next()).await }).unwrap().unwrap().unwrap();
        serde_json::from_str(m.to_text().unwrap()).unwrap()
    };
    let owner = first(&old);
    assert_eq!((owner["session_id"].as_str(), owner["admin"].as_bool(), owner["user"]["user"].as_str()), (Some("owner-main"), Some(true), Some("owner")));
    let dana = first(&member);
    assert_eq!((dana["session_id"].as_str(), dana["admin"].as_bool(), dana["user"]["name"].as_str()), (Some("oid-dana-main"), Some(false), Some("Dana")));

    let http = reqwest::blocking::Client::new();
    let get = |path: &str, token: &str| http.get(format!("http://{addr}{path}")).bearer_auth(token).send().unwrap();
    // Backups are admins'.
    assert_eq!(get("/api/backups/latest", &member).status(), 403);
    assert_ne!(get("/api/backups/latest", &old).status(), 403);
    // A file is its sender's (and admins').
    let sent: Value = http.post(format!("http://{addr}/api/files")).bearer_auth(&old).header("x-filename", "notes.txt").body("secret").send().unwrap().json().unwrap();
    let id = sent["id"].as_str().unwrap();
    assert_eq!(get(&format!("/api/files/{id}"), &old).status(), 200);
    assert_eq!(get(&format!("/api/files/{id}"), &member).status(), 404, "someone else's file isn't there");
    // Who am I.
    let me: Value = get("/api/me", &member).json().unwrap();
    assert_eq!((me["user"]["user"].as_str(), me["user"]["admin"].as_bool()), (Some("oid-dana"), Some(false)));
    // The token works as a subprotocol too (kept out of URLs).
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{addr}/ws").into_client_request().unwrap();
    req.headers_mut().insert("Sec-WebSocket-Protocol", format!("lyra, {member}").parse().unwrap());
    let (mut open_ws, _) = rt.block_on(tokio_tungstenite::connect_async(req)).unwrap();
    let first = rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), open_ws.next()).await }).unwrap().unwrap().unwrap();
    assert!(first.to_text().unwrap().contains("oid-dana-main"));
    // A disabled account is shut out at once, also where it's already connected.
    Users::open(&dir).update("dana", None, Some(Status::Disabled)).unwrap();
    assert_eq!(get("/api/me", &member).status(), 403);
    assert!(rt.block_on(tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={member}"))).is_err());
    use futures_util::SinkExt;
    rt.block_on(open_ws.send(tokio_tungstenite::tungstenite::Message::Text(json!({ "type": "ping" }).to_string().into()))).unwrap();
    let next = rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), open_ws.next()).await }).unwrap();
    assert!(!matches!(next, Some(Ok(tokio_tungstenite::tungstenite::Message::Text(ref t))) if t.contains("pong")), "no answer for a turned-off account: {next:?}");
    // A sign-in code is only good in the browser that started the sign-in.
    let redeemed = http.post(format!("http://{addr}/api/auth/redeem")).json(&json!({ "code": "made-up" })).send().unwrap();
    assert_eq!(redeemed.status(), 403);
}
