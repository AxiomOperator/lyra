use super::*;
use tools::approval;
use crate::acting::run as as_;

const OWNER: &str = lyra_web::users::OWNER;

/// The owner and Dana (a member), in a test home.
fn dana(home: &std::path::Path) -> String {
    std::fs::create_dir_all(home.join("web")).unwrap();
    let users = lyra_web::Users::open(&home.join("web"));
    users.ensure_owner("Garrett").unwrap();
    users.create_local("dana", "Dana Whitfield", lyra_web::users::Role::Member).unwrap().0.id
}

#[test]
fn own_tasks_with_dates_reminders_and_repeats() {
    crate::config::with_test_home(|_| {
        let t = as_(OWNER, || add(New { title: "Pay the invoice".into(), due: "2026-10-01".into(), remind: "2026-10-01 15:00".into(), repeat: "monthly".into(), priority: "high".into(), ..Default::default() })).unwrap();
        assert_eq!((t.due.as_deref(), t.repeat.as_deref(), t.priority.as_str()), (Some("2026-10-01"), Some("monthly"), "high"));
        let mine = as_(OWNER, || list(&Scope::Mine, "any", false)).unwrap();
        assert_eq!(mine.len(), 1);
        assert!(as_(OWNER, || reminder_of(&t.id)).is_some(), "their own reminder");
        // Done: the next one comes, a month on (never in the past), its reminder with it.
        let (done, next) = as_(OWNER, || complete(&t.id, "paid")).unwrap();
        assert_eq!(done.status, "done");
        let next = next.expect("a repeating task comes back");
        assert!(next.due.as_deref().unwrap() >= Local::now().date_naive().to_string().as_str(), "{:?}", next.due);
        assert!(as_(OWNER, || reminder_of(&next.id)).is_some(), "the reminder moved with it");
        assert!(as_(OWNER, || reminder_of(&t.id)).is_none());
        let open = as_(OWNER, || list(&Scope::Mine, "any", false)).unwrap();
        assert_eq!(open.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), [next.id.as_str()]);
        assert!(as_(OWNER, || history(&t.id)).unwrap().iter().any(|e| e.kind == "done" && e.text == "paid"));
        assert_eq!(as_(OWNER, || search("invoice")).unwrap().len(), 2, "open and done");
        // Changes, then gone from the lists.
        let u = as_(OWNER, || update(&next.id, Change { due: Some("none".into()), status: Some("in progress".into()), ..Default::default() })).unwrap();
        assert_eq!((u.due, u.status.as_str()), (None, "in_progress"));
        as_(OWNER, || delete(&next.id)).unwrap();
        assert!(as_(OWNER, || list(&Scope::Mine, "any", false)).unwrap().is_empty());
        assert!(as_(OWNER, || add(New::default())).is_err(), "a task needs a title");
        assert!(as_(OWNER, || add(New { title: "x".into(), project: "none".into(), due: "none".into(), repeat: "none".into(), ..Default::default() })).is_ok(), "the page's \"none\" is no project");
        assert!(as_(OWNER, || add(New { title: "x".into(), repeat: "fortnightly-ish".into(), ..Default::default() })).is_err());
    });
}

#[test]
fn a_task_given_to_someone_waits_for_their_answer_and_stays_private_otherwise() {
    crate::config::with_test_home(|home| {
        let dana = dana(home);
        take_notices();
        let mine = as_(OWNER, || add(New { title: "My own thing".into(), ..Default::default() })).unwrap();
        let t = as_(OWNER, || add(New { title: "Order toner".into(), due: "friday".into(), assignees: vec!["Dana".into()], ..Default::default() })).unwrap();
        // Dana's asked, with Accept / Decline on the push.
        let n = take_notices();
        assert!(n.iter().any(|n| n.to == dana && n.actions.iter().any(|(a, _)| a == "accept")), "{n:?}");
        // Dana sees it waiting, not the owner's own task.
        assert_eq!(as_(&dana, || list(&Scope::Waiting, "any", false)).unwrap().len(), 1);
        assert!(as_(&dana, || get(&mine.id)).is_err(), "someone else's own task isn't seen");
        assert!(as_(&dana, || list(&Scope::Mine, "any", false)).unwrap().is_empty());
        assert!(as_(&dana, || update(&t.id, Change { title: Some("x".into()), ..Default::default() })).unwrap_err().contains("accept"));
        // Accepted: hers now, and the owner sees it as given.
        as_(&dana, || answer(&t.id, true, "")).unwrap();
        assert!(take_notices().iter().any(|n| n.to == OWNER && n.title.contains("accepted")));
        assert_eq!(as_(&dana, || list(&Scope::Mine, "any", false)).unwrap().len(), 1);
        assert_eq!(as_(OWNER, || list(&Scope::Given, "any", false)).unwrap().len(), 1);
        assert!(!as_(OWNER, || list(&Scope::Mine, "any", false)).unwrap().iter().any(|x| x.id == t.id));
        // Shared now: a change from the chat needs the owner's Allow; their own doesn't.
        assert!(as_(OWNER, || approval("task_complete", &json!({ "id": t.id }))).is_some());
        assert!(as_(OWNER, || approval("task_complete", &json!({ "id": mine.id }))).is_none());
        assert!(as_(OWNER, || approval("task_add", &json!({ "title": "x", "assignees": ["Dana"] }))).is_some());
        assert!(as_(OWNER, || approval("task_add", &json!({ "title": "x" }))).is_none());
        // Dana finishes it: the owner hears.
        as_(&dana, || complete(&t.id, "")).unwrap();
        assert!(take_notices().iter().any(|n| n.to == OWNER && n.title.contains("finished")));
        // Declined: back with its owner.
        let t2 = as_(OWNER, || add(New { title: "Fix the printer".into(), assignees: vec!["dana".into()], ..Default::default() })).unwrap();
        as_(&dana, || answer(&t2.id, false, "not mine")).unwrap();
        assert!(as_(OWNER, || list(&Scope::Mine, "any", false)).unwrap().iter().any(|x| x.id == t2.id));
        assert!(as_(&dana, || get(&t2.id)).is_err(), "declined: not hers to see");
        assert!(as_(&dana, || answer(&t2.id, true, "")).is_err(), "answered already");
        assert!(as_(OWNER, || add(New { title: "x".into(), assignees: vec!["nobody here".into()], ..Default::default() })).is_err());
    });
}

#[test]
fn projects_have_members_tasks_and_status_updates() {
    crate::config::with_test_home(|home| {
        let dana = dana(home);
        let p = as_(OWNER, || add_project("Website", "the new site", &["Dana".into()], "none")).unwrap();
        assert!(p.member(&dana));
        let t = as_(OWNER, || add(New { title: "Write the copy".into(), project: "website".into(), ..Default::default() })).unwrap();
        // Dana (a member) sees and changes the project's task.
        assert!(as_(&dana, || get(&t.id)).is_ok());
        assert_eq!(as_(&dana, || list(&Scope::parse("project Website").unwrap(), "any", false)).unwrap().len(), 1);
        assert!(as_(&dana, || update(&t.id, Change { priority: Some("urgent".into()), ..Default::default() })).is_ok());
        // Others in it: the chat asks first.
        assert!(as_(OWNER, || approval("task_post_update", &json!({ "project": "Website", "health": "at_risk", "text": "late" }))).is_some());
        let draft = as_(OWNER, || draft_update("Website")).unwrap();
        assert_eq!(draft["health"], "on_track");
        as_(OWNER, || post_update("Website", "at risk", "The copy is late.")).unwrap();
        let b = as_(OWNER, || project_brief(&project("Website").unwrap()));
        assert_eq!((b["health"].as_str(), b["tasks"]["open"].as_u64()), (Some("at_risk"), Some(1)));
        assert_eq!(b["last_update"]["text"], "The copy is late.");
        // Only the owner changes who's in it; a member can't delete it.
        assert!(as_(&dana, || update_project("Website", ProjectChange { remove_members: vec!["Garrett".into()], ..Default::default() })).is_err());
        assert!(as_(&dana, || delete_project("Website")).is_err());
        // Someone not in it doesn't see it (an admin does).
        as_(OWNER, || update_project("Website", ProjectChange { remove_members: vec!["Dana".into()], ..Default::default() })).unwrap();
        assert!(as_(&dana, projects).unwrap().is_empty());
        assert!(as_(&dana, || get(&t.id)).is_err());
        assert!(as_(OWNER, || add_project("website", "", &[], "")).is_err(), "names are one each");
    });
}

#[test]
fn reminders_go_off_once_then_again_while_open() {
    crate::config::with_test_home(|_| {
        configure(Settings { enabled: true, nag: true, nag_minutes: 30, nag_max: 2 });
        let soon = (Local::now() + Duration::minutes(1)).format("%Y-%m-%d %H:%M").to_string();
        let t = as_(OWNER, || add(New { title: "Call the vendor".into(), remind: soon, ..Default::default() })).unwrap();
        let quiet = |_: &str| false;
        let start = Utc::now();
        assert!(remind::due(start, &quiet).is_empty(), "not yet");
        let at = start + Duration::minutes(2);
        let first = remind::due(at, &quiet);
        assert_eq!(first.iter().map(|d| (d.task.as_str(), d.nag)).collect::<Vec<_>>(), [(t.id.as_str(), 0)]);
        assert!(remind::due(at + Duration::minutes(10), &quiet).is_empty(), "not again so soon");
        // Quiet time: the repeat waits.
        assert!(remind::due(at + Duration::minutes(31), &|_| true).is_empty());
        assert_eq!(remind::due(at + Duration::minutes(31), &quiet)[0].nag, 1);
        assert_eq!(remind::due(at + Duration::minutes(62), &quiet)[0].nag, 2);
        assert!(remind::due(at + Duration::minutes(93), &quiet).is_empty(), "at most nag_max");
        // The push's buttons: in an hour sets it again; done stops it.
        let said = as_(OWNER, || remind::push_action("snooze1h", &t.id)).unwrap();
        assert!(said.contains("reminder moved"), "{said}");
        as_(OWNER, || remind::push_action("done", &t.id)).unwrap();
        assert!(remind::due(at + Duration::hours(3), &quiet).is_empty(), "done: no more reminders");
        configure(Settings::default());
    });
}

#[test]
fn the_old_pmi_names_still_work() {
    crate::config::with_test_home(|_| {
        assert_eq!(tools::alias("pmi_add_task"), Some("task_add"));
        let added = as_(OWNER, || tools::call("pmi_add_task", &json!({ "title": "Old habit", "where": "" }))).unwrap();
        let listed = as_(OWNER, || tools::call("pmi_tasks", &json!({}))).unwrap();
        assert_eq!(listed["tasks"][0]["id"], added["id"]);
        assert!(as_(OWNER, || tools::call("pmi_add_risk", &json!({}))).is_err());
        // The commands, by the number in the last list.
        assert!(as_(OWNER, || tools::tasks_text("")).unwrap().contains("Old habit"));
        assert!(as_(OWNER, || tools::task_command("done 1")).unwrap().contains("done: Old habit"));
        assert!(as_(OWNER, || tools::task_command("add call Dana friday 3pm")).unwrap().contains("reminder"));
    });
}
