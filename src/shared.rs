//! Shared conversations: someone invites a coworker into a conversation
//! (`/share <person>`, or Share in the app). Each person has their own copy
//! (their own saved session, in their own list), and lyra keeps the copies
//! the same as they go: both see the replies stream in and either can add.
//! Each message runs as whoever sent it, with their own rights, tools and
//! accounts, never the other person's; what lyra learns from it is theirs.
//! The people in each one are kept in `sessions/threads.json`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One person in a shared conversation, and their copy of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Seat {
    pub user: String,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    /// Who started it (and may take people out).
    pub host: String,
    pub people: Vec<Seat>,
    pub created: DateTime<Utc>,
}

fn store() -> Option<crate::store::JsonStore<Vec<Thread>>> {
    Some(crate::store::JsonStore::new(crate::sessions::dir()?.join("threads.json")))
}

pub fn get(id: &str) -> Option<Thread> {
    store()?.load().into_iter().find(|t| t.id == id)
}

/// This person's copy of a thread, while they're in it.
pub fn seat(thread: &str, user: &str) -> Option<Seat> {
    get(thread)?.people.into_iter().find(|s| s.user == user)
}

/// The others' copies.
pub fn others(thread: &str, user: &str) -> Vec<Seat> {
    get(thread).map(|t| t.people.into_iter().filter(|s| s.user != user).collect()).unwrap_or_default()
}

/// Add someone (the thread is made the first time): their copy's session id.
pub fn add(thread: &str, host: &str, host_session: &str, guest: &str, guest_session: &str) -> Result<Thread, String> {
    let store = store().ok_or("no home directory")?;
    store.try_update(|all| {
        if !all.iter().any(|t| t.id == thread) {
            all.push(Thread { id: thread.into(), host: host.into(), people: vec![Seat { user: host.into(), session: host_session.into() }], created: Utc::now() });
        }
        let t = all.iter_mut().find(|t| t.id == thread).expect("just made");
        if t.people.iter().any(|s| s.user == guest) {
            return Err(format!("{} is already in it", crate::tasks::name_of(guest)));
        }
        t.people.push(Seat { user: guest.into(), session: guest_session.into() });
        Ok(t.clone())
    })
}

/// Take someone out (the host takes anyone out, anyone leaves). Their copy stays theirs, on its own.
pub fn remove(thread: &str, by: &str, who: &str) -> Result<Thread, String> {
    let store = store().ok_or("no home directory")?;
    store.try_update(|all| {
        let t = all.iter_mut().find(|t| t.id == thread).ok_or("this conversation isn't shared")?;
        if by != t.host && by != who {
            return Err("only the person who shared it takes others out (you can leave: /unshare me)".into());
        }
        if who == t.host {
            return Err("the person who shared it stays in it: take the others out instead".into());
        }
        let n = t.people.len();
        t.people.retain(|s| s.user != who);
        if t.people.len() == n {
            return Err(format!("{} isn't in it", crate::tasks::name_of(who)));
        }
        Ok(t.clone())
    })
}

/// Who's in it, by name ("Garrett Post, Dana Whitfield").
pub fn names(thread: &str) -> Vec<String> {
    get(thread).map(|t| t.people.iter().map(|s| crate::tasks::name_of(&s.user)).collect()).unwrap_or_default()
}

/// Copy `from` into `to`, keeping each message's run where it was made (a
/// reply is rated in the copy it was made in).
fn sync_into(to: &mut Vec<crate::Message>, from: &[crate::Message]) -> usize {
    // From where they differ (what the devices are sent again).
    let first = to.iter().zip(from).position(|(a, b)| a.role != b.role || a.content != b.content || a.reasoning != b.reasoning || a.tool_calls.len() != b.tool_calls.len()).unwrap_or(to.len().min(from.len()));
    let runs: Vec<Option<crate::LastRun>> = to.iter().map(|m| m.run.clone()).collect();
    *to = from.iter().map(crate::Message::copy).collect();
    for (m, run) in to.iter_mut().zip(runs) {
        if run.is_some() {
            m.run = run;
        }
    }
    first
}

impl crate::App {
    /// The same shared conversation, changed elsewhere: this copy becomes the same.
    pub(crate) fn take_shared(&mut self, from: &[crate::Message]) {
        let first = sync_into(&mut self.messages, from);
        self.touched = Some(self.touched.map_or(first, |t| t.min(first)));
        self.scroll = None;
    }

    /// A fingerprint of what a copy shows (to see it changed): messages, and the last one's length.
    pub(crate) fn shared_mark(&self) -> (usize, usize, usize, bool) {
        let last = self.messages.last();
        (self.messages.len(), last.map_or(0, |m| m.content.len() + m.reasoning.len()), last.map_or(0, |m| m.tool_calls.len()), self.waiting)
    }

    /// `/share [person]`, `/unshare <person>`: who's in this conversation, someone in, someone out.
    pub(crate) fn share_command(&mut self, cmd: &str, arg: &str) -> Result<String, String> {
        let arg = arg.trim();
        if self.hub.is_none() {
            return Err("sharing works in lyra serve (the app), where both of you can see it".into());
        }
        if cmd == "/unshare" {
            let thread = self.thread.clone().ok_or("this conversation isn't shared")?;
            let who = if arg.is_empty() || arg == "me" { self.owner.clone() } else { crate::tasks::person(arg)? };
            remove(&thread, &self.owner, &who)?;
            let name = crate::tasks::name_of(&who);
            if who == self.owner {
                self.thread = None;
                self.save_session();
                return Ok("you left the shared conversation: your copy stays yours, on its own".into());
            }
            if let Some(hub) = &self.hub {
                hub.notify(lyra_web::Notification {
                    title: format!("{} stopped sharing a conversation with you", crate::tasks::name_of(&self.owner)),
                    body: "Your copy stays yours, as it was.".into(),
                    tag: "shared".into(),
                    approval: None,
                    url: None,
                    actions: vec![],
                    reference: None,
                    to: lyra_web::To::User(who.clone()),
                });
            }
            self.log(crate::Level::Info, format!("{name} taken out of this shared conversation"));
            return Ok(format!("{name} is out of it: their copy stays as it was, and they don't see what's said from now on"));
        }
        if arg.is_empty() {
            return Ok(match &self.thread {
                Some(t) => format!("shared with: {}\n/share <person> adds someone · /unshare <person> takes them out", names(t).join(", ")),
                None => "not shared. /share <person> invites someone: they see it as it goes and can add to it (each message runs with its sender's own rights)".into(),
            });
        }
        let guest = crate::tasks::person(arg)?;
        if guest == self.owner {
            return Err("that's you".into());
        }
        if !self.messages.iter().any(|m| m.role == "user") {
            return Err("say something first: an empty conversation has nothing to share".into());
        }
        if let Some(t) = &self.thread
            && get(t).is_some_and(|t| t.host != self.owner)
        {
            return Err("only the person who shared it invites others".into());
        }
        let thread = self.thread.clone().unwrap_or_else(|| self.session_id.clone());
        let dir = crate::sessions::dir().ok_or("no home directory")?;
        // Their copy: the conversation so far, in their own list.
        let id = crate::sessions::new_id();
        let mut copy = crate::sessions::Session::from_messages(&id, self.session_started, &self.messages);
        copy.owner = guest.clone();
        copy.thread = Some(thread.clone());
        copy.title = format!("{} (with {})", copy.title, crate::tasks::name_of(&self.owner));
        add(&thread, &self.owner, &self.session_id, &guest, &id)?;
        crate::sessions::save(&dir, &copy)?;
        self.thread = Some(thread.clone());
        self.save_session();
        let name = crate::tasks::name_of(&guest);
        if let Some(hub) = &self.hub {
            hub.notify(lyra_web::Notification {
                title: format!("{} shared a conversation with you", crate::tasks::name_of(&self.owner)),
                body: copy.title.clone(),
                tag: "shared".into(),
                approval: None,
                url: Some(format!("/?open={id}")),
                actions: vec![],
                reference: None,
                to: lyra_web::To::User(guest.clone()),
            });
        }
        self.log(crate::Level::Info, format!("shared this conversation with {name}"));
        Ok(format!("shared with {name}: they see it as it goes and can add to it. Each message runs with its sender's own rights and accounts. /unshare {name} takes them out"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_stay_the_same_and_each_keeps_its_own_ratings() {
        let mut mine = vec![crate::Message::new("user", "hi".into())];
        mine[0].by = "Garrett".into();
        let mut theirs: Vec<crate::Message> = Vec::new();
        assert_eq!(sync_into(&mut theirs, &mine), 0);
        assert_eq!((theirs.len(), theirs[0].by.as_str(), theirs[0].said().as_str()), (1, "Garrett", "Garrett: hi"));
        // Their reply was made (and is rated) in their copy: it keeps its run when mine catches up.
        theirs.push(crate::Message::new("assistant", "hello".into()));
        theirs[1].run = Some(crate::LastRun::new(lyra_memory::Uuid::new_v4(), vec![], 0, None));
        let source: Vec<crate::Message> = theirs.iter().map(crate::Message::copy).collect();
        assert!(source[1].run.is_none(), "a copy doesn't take the run with it");
        assert_eq!(sync_into(&mut theirs, &source), 2, "nothing differs");
        assert!(theirs[1].run.is_some());
    }
}
