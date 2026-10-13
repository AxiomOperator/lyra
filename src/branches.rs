//! Branching conversations: change any of your earlier messages and send it
//! again, and the conversation takes another path from there, with the way
//! it went before kept as a version. A fork is a message with versions (each
//! the rest of the conversation from it, and the forks further on in it);
//! the live one is what the conversation shows, the others wait, and a
//! switch (‹ 1/2 ›) swaps them. Kept with the saved conversation.

use serde::{Deserialize, Serialize};

use crate::{App, Level, Message};

/// The rest of the conversation from a fork, one way it went.
#[derive(Default)]
pub(crate) struct Version {
    messages: Vec<Message>,
    /// The forks further on in it.
    forks: Vec<Fork>,
}

pub(crate) struct Fork {
    /// The user message where the versions part (its index).
    pub at: usize,
    /// The live one's place is left empty: it's in the conversation.
    versions: Vec<Version>,
    pub current: usize,
}

impl Fork {
    pub fn count(&self) -> usize {
        self.versions.len()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedVersion {
    pub messages: Vec<crate::sessions::SavedMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forks: Vec<SavedFork>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedFork {
    pub at: usize,
    pub current: usize,
    pub versions: Vec<SavedVersion>,
}

pub(crate) fn save(forks: &[Fork]) -> Vec<SavedFork> {
    forks
        .iter()
        .map(|f| SavedFork { at: f.at, current: f.current, versions: f.versions.iter().map(|v| SavedVersion { messages: v.messages.iter().map(crate::sessions::saved).collect(), forks: save(&v.forks) }).collect() })
        .collect()
}

pub(crate) fn restore(saved: Vec<SavedFork>) -> Vec<Fork> {
    saved
        .into_iter()
        .filter(|f| f.current < f.versions.len())
        .map(|f| Fork { at: f.at, current: f.current, versions: f.versions.into_iter().map(|v| Version { messages: v.messages.into_iter().map(crate::sessions::restored).collect(), forks: restore(v.forks) }).collect() })
        .collect()
}

/// The conversation from `at` on (and the forks in it), taken out as a version.
fn take_from(messages: &mut Vec<Message>, forks: &mut Vec<Fork>, at: usize) -> Version {
    let tail = messages.split_off(at.min(messages.len()));
    let (inner, outer): (Vec<Fork>, Vec<Fork>) = std::mem::take(forks).into_iter().partition(|f| f.at > at);
    *forks = outer;
    Version { messages: tail, forks: inner }
}

/// A new version at `at`: the way it went is kept, the conversation goes on from before it.
pub(crate) fn part(messages: &mut Vec<Message>, forks: &mut Vec<Fork>, at: usize) {
    let old = take_from(messages, forks, at);
    match forks.iter_mut().find(|f| f.at == at) {
        Some(f) => {
            f.versions[f.current] = old;
            f.versions.push(Version::default());
            f.current = f.versions.len() - 1;
        }
        None => forks.push(Fork { at, versions: vec![old, Version::default()], current: 1 }),
    }
}

/// Show version `k` (from 0) of the fork at `at`.
pub(crate) fn switch(messages: &mut Vec<Message>, forks: &mut Vec<Fork>, at: usize, k: usize) -> Result<(), String> {
    let i = forks.iter().position(|f| f.at == at).ok_or("that message has no other versions")?;
    if k >= forks[i].versions.len() {
        return Err(format!("it has {} versions", forks[i].versions.len()));
    }
    if k == forks[i].current {
        return Ok(());
    }
    let mut f = forks.remove(i);
    f.versions[f.current] = take_from(messages, forks, at);
    let v = std::mem::take(&mut f.versions[k]);
    messages.extend(v.messages);
    forks.extend(v.forks);
    f.current = k;
    forks.push(f);
    forks.sort_by_key(|f| f.at);
    Ok(())
}

impl App {
    /// Earlier turns summarized past `at`: the summary no longer fits what's there.
    fn compacted_fits(&mut self, at: usize) {
        let before = self.messages[..at.min(self.messages.len())].iter().filter(|m| m.is_history()).count();
        if self.compacted.as_ref().is_some_and(|(upto, _)| *upto > before) {
            self.compacted = None;
        }
    }

    /// `/branch <index> <text>`: an earlier message of theirs, changed and sent
    /// again; the way it went is kept as a version.
    pub(crate) fn branch(&mut self, at: usize, text: &str) -> Result<(), String> {
        let text = text.trim();
        if self.waiting {
            return Err("wait for the reply to finish".into());
        }
        if self.thread.is_some() {
            return Err("a shared conversation goes one way: start a new one to try another".into());
        }
        let m = self.messages.get(at).filter(|m| m.role == "user").ok_or("that isn't one of your messages")?;
        if text.is_empty() || text == m.content.trim() {
            return Err("change it first".into());
        }
        // What was attached stays with it.
        let text = match m.content.find("\n\n**Attached: ") {
            Some(i) => format!("{text}{}", &m.content[i..]),
            None => text.to_string(),
        };
        let images = m.images.clone();
        part(&mut self.messages, &mut self.forks, at);
        self.compacted_fits(at);
        self.can_continue = false;
        self.last_run = None;
        self.touched = Some(at);
        self.log(Level::Info, format!("message {at} changed: another version from there (the earlier one is kept)"));
        self.input = text;
        self.attach_images = images;
        self.send();
        self.attach_images.clear();
        Ok(())
    }

    /// `/version <index> <n>`: show another version from that message (n from 1).
    pub(crate) fn show_version(&mut self, at: usize, n: usize) -> Result<(), String> {
        if self.waiting {
            return Err("wait for the reply to finish".into());
        }
        switch(&mut self.messages, &mut self.forks, at, n.checked_sub(1).ok_or("versions count from 1")?)?;
        self.compacted_fits(at);
        self.can_continue = false;
        // The last reply shown can be rated again (its run is kept with it).
        let last_user = self.messages.iter().rposition(|m| m.role == "user");
        self.last_run = self.messages.iter().enumerate().rev().find(|(i, m)| m.run.is_some() && last_user.is_none_or(|u| *i > u)).and_then(|(_, m)| m.run.clone());
        self.touched = Some(at);
        self.scroll = None;
        self.save_session();
        Ok(())
    }

    /// The forks the conversation shows now: (where, how many, which).
    pub(crate) fn fork_view(&self) -> Vec<(usize, usize, usize)> {
        self.forks.iter().filter(|f| f.at < self.messages.len()).map(|f| (f.at, f.count(), f.current)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(m: &[Message]) -> Vec<&str> {
        m.iter().map(|m| m.content.as_str()).collect()
    }

    #[test]
    fn each_way_it_went_is_kept_and_comes_back() {
        let say = |t: &str, r: &str| vec![Message::new("user", t.into()), Message::new("assistant", r.into())];
        let mut messages: Vec<Message> = [say("a", "A"), say("b", "B"), say("c", "C")].into_iter().flatten().collect();
        let mut forks = Vec::new();
        // b changed: the conversation goes on from a.
        part(&mut messages, &mut forks, 2);
        messages.extend(say("b2", "B2"));
        assert_eq!(texts(&messages), ["a", "A", "b2", "B2"]);
        // A fork further on in the new version.
        messages.extend(say("d", "D"));
        part(&mut messages, &mut forks, 4);
        messages.extend(say("d2", "D2"));
        assert_eq!(forks.iter().map(|f| (f.at, f.count(), f.current)).collect::<Vec<_>>(), [(2, 2, 1), (4, 2, 1)]);
        // Back to the first way at b: the later fork goes with its version.
        switch(&mut messages, &mut forks, 2, 0).unwrap();
        assert_eq!(texts(&messages), ["a", "A", "b", "B", "c", "C"]);
        assert_eq!(forks.iter().map(|f| f.at).collect::<Vec<_>>(), [2]);
        // And back again: as it was left, its own fork too.
        switch(&mut messages, &mut forks, 2, 1).unwrap();
        assert_eq!(texts(&messages), ["a", "A", "b2", "B2", "d2", "D2"]);
        switch(&mut messages, &mut forks, 4, 0).unwrap();
        assert_eq!(texts(&messages), ["a", "A", "b2", "B2", "d", "D"]);
        // Kept with the conversation, and read back the same.
        let back = restore(serde_json::from_str(&serde_json::to_string(&save(&forks)).unwrap()).unwrap());
        assert_eq!(back.iter().map(|f| (f.at, f.count(), f.current)).collect::<Vec<_>>(), [(2, 2, 1), (4, 2, 0)]);
        assert!(switch(&mut messages, &mut forks, 3, 0).is_err() && switch(&mut messages, &mut forks, 2, 5).is_err());
    }
}
