//! Subagents end to end: profiles and templates, the registry and versions,
//! routing, the wizard, the delegation contract and permissions.

use lyra_agents::builder::{AgentCreationDraft, CreationMode, Stage};
use lyra_agents::delegation::{self, DelegationStatus};
use lyra_agents::router::{self, RouteMethod};
use lyra_agents::*;
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};

fn registry() -> (tokio::runtime::Runtime, AgentRegistry, std::path::PathBuf) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = std::env::temp_dir().join(format!("lyra-agents-{}", uuid::Uuid::new_v4()));
    let r = AgentRegistry::open(&dir, rt.handle().clone()).unwrap();
    (rt, r, dir)
}

#[test]
fn templates_are_valid_and_defaults_install() {
    for name in templates::NAMES.iter().filter(|n| **n != "custom") {
        let p = templates::template(name).unwrap();
        validate(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let (_rt, r, dir) = registry();
    let names: Vec<String> = r.list().0.into_iter().map(|a| a.name).collect();
    assert_eq!(names, ["archivist", "researcher"], "the planner's helpers are there from the start");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn profiles_are_files_with_versions() {
    let (_rt, r, dir) = registry();
    let writer = r.create(templates::template("writer").unwrap(), "created").unwrap();
    assert!(dir.join("writer.toml").exists());
    assert!(r.create(writer.clone(), "again").is_err(), "names are unique");
    let mut p = r.find("Writer").unwrap();
    p.instructions += "\nNever use exclamation marks.";
    let v2 = r.update(p, "user changed the tone").unwrap();
    assert_eq!(v2.version, 2);
    // Edited by hand: a new version on sync.
    let text = std::fs::read_to_string(dir.join("writer.toml")).unwrap().replace("Professional writing assistant.", "Plain-spoken writer.");
    std::fs::write(dir.join("writer.toml"), text).unwrap();
    assert!(r.sync().iter().any(|n| n.contains("edited by hand")));
    assert_eq!(r.get("writer").unwrap().version, 3);
    let back = r.rollback("writer", Some(1)).unwrap();
    assert_eq!((back.version, back.role.as_str()), (4, "Professional writing assistant."), "a rollback is a new version");
    assert_eq!(r.versions("writer").unwrap().len(), 4);
    r.delete("writer").unwrap();
    assert!(r.get("writer").is_none());
    assert!(r.rollback("writer", Some(4)).is_ok(), "deleted agents can be restored");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rules_route_the_obvious_and_respect_exclusions() {
    let writer = templates::template("writer").unwrap();
    let agents = vec![writer, templates::template("researcher").unwrap()];
    let d = router::by_rules(&agents, "Please rewrite this email for me:\n\nWe will be having an outage", 0.75).unwrap();
    assert_eq!((d.agent.as_str(), d.method), ("writer", RouteMethod::Rule));
    assert!(d.confidence >= 0.75);
    assert!(router::by_rules(&agents, "write source code that parses CSV and rewrite it in rust", 0.75).is_none(), "excluded");
    assert!(router::by_rules(&agents, "what's the weather like", 0.75).is_none());
    assert!(router::by_rules(&agents, "find out what we know about backups", 0.5).is_none(), "the researcher doesn't take work on its own");
    let e = router::explicit(&agents, "ask the researcher what we know about backups").unwrap();
    assert_eq!((e.agent.as_str(), e.method), ("researcher", RouteMethod::Explicit));
    let refs: Vec<&AgentProfile> = agents.iter().collect();
    assert_eq!(router::parse_route(r#"{"agent":"Writer","confidence":0.8,"reason":"tone"}"#, &refs).unwrap().agent, "writer");
    assert_eq!(router::parse_route(r#"{"agent":"main","confidence":0.9}"#, &refs).unwrap().agent, "main");
    assert!(router::parse_route(r#"{"agent":"chef"}"#, &refs).is_err());
}

#[test]
fn the_wizard_builds_a_profile_from_a_template_or_from_scratch() {
    let mut d = AgentCreationDraft::start("Create a Writer Agent");
    assert_eq!((d.mode, d.template.as_deref()), (CreationMode::Template, Some("writer")));
    let mut asked = Vec::new();
    while let Some(q) = d.next_question() {
        asked.push(q.key);
        let answer = match q.key {
            "tone" => "2, 1",
            "behavior" => "1",
            "memory" => "2",
            _ => "1",
        };
        d.answer(answer).unwrap();
    }
    assert_eq!(asked, ["tone", "behavior", "memory", "delegation"], "a template only asks how to customize it");
    let p = d.profile();
    assert_eq!((p.name.as_str(), p.delegation.auto_delegate, p.memory_policy.mode), ("writer", true, MemoryMode::Scoped));
    assert!(p.instructions.contains("concise, professional"));
    assert!(d.summary().contains("Writer Agent") && d.summary().contains("Auto delegation:\nenabled"));

    let mut g = AgentCreationDraft::start("create a new subagent");
    assert_eq!(g.mode, CreationMode::Guided);
    assert_eq!(g.next_question().unwrap().key, "name");
    g.answer("Support Agent").unwrap();
    g.answer("answering customer questions about billing").unwrap();
    g.answer("3").unwrap();
    g.answer("skip").unwrap();
    g.answer("2").unwrap();
    g.answer("1").unwrap();
    g.answer("no").unwrap();
    assert!(g.answer("2").is_err(), "another model needs a name");
    g.answer("qwen-small").unwrap();
    assert!(g.next_question().is_none());
    let p = g.profile();
    assert_eq!((p.name.as_str(), p.title.as_str()), ("support", "Support"));
    assert_eq!(p.memory_policy.mode, MemoryMode::None);
    assert!(p.tools.is_empty(), "no memory, so no memory tools");
    assert_eq!(p.model_policy.model.as_deref(), Some("qwen-small"));
    let p = builder::apply_generated(p, r#"{"role":"Billing support","instructions":"Answer billing questions.","examples":["Why was I charged twice?"],"test_task":"Explain a refund"}"#).unwrap();
    assert_eq!((p.role.as_str(), p.test_task.as_deref()), ("Billing support", Some("Explain a refund")));
    assert_eq!(g.stage, Stage::Interview);
}

#[test]
fn expert_profiles_from_toml_or_yaml() {
    let toml = "name = \"ops\"\ndescription = \"Runs operations\"\ntools = [\"memory_recall\"]\n[routing]\nauto_delegate = true\nexamples = [\"restart the service\"]";
    let p = builder::from_expert(toml).unwrap();
    assert_eq!((p.name.as_str(), p.delegation.auto_delegate), ("ops", true));
    let yaml = "name: Translator\ndescription: Translates text\nrole: Translator\npermissions:\n  max_risk: read_only\n";
    assert_eq!(builder::from_expert(yaml).unwrap().name, "translator");
    assert!(builder::from_expert("name = \"x\"").is_err());
}

#[test]
fn delegation_contract_and_permissions() {
    let r = delegation::parse_result(uuid::Uuid::new_v4(), "Dear team,\nThe server is down.\nCONFIDENCE: 0.9");
    assert_eq!((r.status, r.confidence, r.text().as_str()), (DelegationStatus::Completed, Some(0.9), "Dear team,\nThe server is down."));
    let refused = delegation::parse_result(uuid::Uuid::new_v4(), "STATUS: refused — this is code, not writing");
    assert_eq!((refused.status, refused.notes.as_deref()), (DelegationStatus::Refused, Some("this is code, not writing")));
    assert_eq!(delegation::extract_input("Please rewrite this email for me:\n\nWe will be having an outage tonight from 9 to 11.").as_deref(), Some("We will be having an outage tonight from 9 to 11."));
    assert_eq!(delegation::extract_input("fix:\n```\nlet x = 1;\n```").as_deref(), Some("let x = 1;"));
    let ctx = delegation::build_context("Rewrite it", Some("hey"), &["prefers concise".into()], &[]);
    assert_eq!(ctx["relevant_memories"][0], "prefers concise");

    let writer = templates::template("writer").unwrap();
    let cap = |name: &str, risk| Capability::new(name, CapabilityKind::NativeTool, "", risk);
    assert!(delegation::allows(&writer, &cap("memory_recall", RiskLevel::ReadOnly)));
    assert!(!delegation::allows(&writer, &cap("memory_remember", RiskLevel::LowWrite)), "not in its tools, above its risk");
    assert!(!delegation::allows(&writer, &cap("pm.tasks.create", RiskLevel::Write)));
    let archivist = templates::template("archivist").unwrap();
    assert!(delegation::allows(&archivist, &cap("memory_remember", RiskLevel::LowWrite)));
    assert!(!delegation::allows(&archivist, &cap("memory_forget", RiskLevel::Destructive)), "above its ceiling");
    assert_eq!(writer.memory_policy.write_scopes("writer").unwrap(), ["agent:writer"]);
    assert_eq!(writer.memory_policy.read_scopes("writer").unwrap(), ["user", "agent:writer"]);
    assert!(templates::template("researcher").unwrap().memory_policy.read_scopes("researcher").is_none(), "shared read: every scope");
}
