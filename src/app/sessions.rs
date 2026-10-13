//! A conversation kept and carried on: saving, resuming, a new one, and the
//! app forked for another conversation or person.

use crate::*;

impl App {
    /// The assistant message being streamed into, created on the first token.
    /// Save this conversation (after every reply and on the way out).
    pub(crate) fn save_session(&mut self) {
        if !self.messages.iter().any(|m| m.role == "user") {
            return;
        }
        let Some(dir) = sessions::dir() else { return };
        let mut s = sessions::Session::from_messages(&self.session_id, self.session_started, &self.messages);
        s.owner = self.owner.clone();
        s.chat_only = self.chat_only;
        s.summary = self.compacted.clone();
        s.effort = self.effort.map(|e| e.as_str().to_string());
        s.thread = self.thread.clone();
        s.forks = branches::save(&self.forks);
        // Written off this thread (a few saves in one turn are one write).
        sessions::save_later(&dir, s);
    }

    /// Carry on a saved conversation (`lyra -c`, `lyra -r <id>`, `/resume <id>`).
    pub(crate) fn resume_session(&mut self, s: sessions::Session) {
        let (id, turns, when) = (s.id.clone(), s.user_turns(), s.updated.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string());
        self.session_id = id.clone();
        self.session_started = s.started;
        self.chat_only = s.chat_only;
        self.compacted = s.summary.clone();
        self.effort = s.effort.as_deref().and_then(turn::Effort::parse);
        // Still in it? (Taken out of a shared conversation: their copy stays, on its own.)
        self.thread = s.thread.clone().filter(|t| crate::shared::seat(t, &self.owner).is_some());
        let mut s = s;
        self.forks = branches::restore(std::mem::take(&mut s.forks));
        self.messages = s.into_messages();
        self.agent_cards.clear();
        self.messages.push(Message::new("info", format!("resumed session {id} · {turns} turns · last active {when}")));
        self.scroll = None;
        // The last reply, if nothing was said after it, can still be judged or rated.
        let last_user = self.messages.iter().rposition(|m| m.role == "user");
        self.last_run = self.messages.iter().enumerate().rev().find(|(i, m)| m.run.is_some() && last_user.is_none_or(|u| *i > u)).and_then(|(_, m)| m.run.clone());
        self.log(Level::Info, format!("resumed session {id} ({turns} turns)"));
    }

    /// `/new`: start a fresh conversation (this one is saved first).
    pub(crate) fn new_session(&mut self) -> Result<String, String> {
        if self.waiting {
            return Err("wait for the reply to finish".into());
        }
        self.save_session();
        self.session_id = sessions::new_id();
        self.session_started = chrono::Utc::now();
        self.messages.clear();
        self.compacted = None;
        self.thread = None;
        self.forks.clear();
        self.effort = None;
        self.agent_cards.clear();
        self.last_run = None;
        self.scroll = None;
        self.log(Level::Info, format!("new session {}", self.session_id));
        Ok(String::new())
    }

    /// `/resume <id>`: switch to a saved conversation (this one is saved first).
    pub(crate) fn resume(&mut self, arg: &str) -> Result<String, String> {
        if self.waiting {
            return Err("wait for the reply to finish".into());
        }
        let dir = sessions::dir().ok_or("no home directory")?;
        if arg.trim().is_empty() {
            return Ok(format!("{}\n\n/resume <id> to switch", sessions::describe(&sessions::list_for(&dir, &self.owner), 20)));
        }
        let s = sessions::find_for(&dir, arg, &self.owner)?;
        if s.id == self.session_id {
            return Err("that's this session".into());
        }
        self.save_session();
        self.resume_session(s);
        Ok(String::new())
    }

    /// Another conversation next to this one (`lyra serve`: one per device or
    /// as asked), sharing memory, skills, agents, tools and the server.
    pub(crate) fn fork(&self) -> App {
        let config = Config::load().unwrap_or_default();
        let mut app = App::new(config, Context::load(), self.shared.clone());
        app.hub = self.hub.clone();
        app.primary = false;
        app.owner = self.owner.clone();
        app.admin = self.admin;
        app.refresh_agents();
        app
    }

    /// The memory scope this conversation is kept to: its person's own
    /// (`user:<id>`) for anyone but the owner.
    pub(crate) fn personal(&self) -> Option<String> {
        (self.owner != lyra_web::users::OWNER).then(|| self.owner.clone())
    }

    /// A new conversation for a user: theirs, with their rights and their USER.md.
    pub(crate) fn fork_for(&self, who: &lyra_web::Who) -> App {
        let config = Config::load().unwrap_or_default();
        let mut app = App::new(config, Context::load_for(&who.user, &who.name), self.shared.clone());
        app.hub = self.hub.clone();
        app.primary = false;
        app.owner = who.user.clone();
        app.admin = who.admin;
        if !crate::acting::is_owner(&who.user) {
            app.goals = goals::for_user(&who.user);
        }
        app.refresh_agents();
        app
    }
}
