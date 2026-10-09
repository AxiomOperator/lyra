//! The people who use lyra (`~/.lyra/web/users.json`): who they are, admin
//! or member, and whether they may sign in. Devices belong to a user; what a
//! device may do comes from its user. The first user is the owner, an admin;
//! devices paired before users existed are theirs.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The first user's id until their Microsoft sign-in claims it.
pub const OWNER: &str = "owner";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Everything, including machines, system tools, devices and users.
    Admin,
    /// Their own conversations, memories, tasks and reminders.
    Member,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Active,
    /// Signed in, waiting for an admin to let them in.
    Pending,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    /// Their Microsoft (Entra) object id; `owner` for the first user until claimed.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub email: String,
    /// Their Microsoft tenant.
    #[serde(default)]
    pub tenant: String,
    /// Their Microsoft (Entra) object id, once they've signed in with it.
    #[serde(default)]
    pub oid: String,
    pub role: Role,
    pub status: Status,
    pub created: DateTime<Utc>,
    #[serde(default)]
    pub last_seen: Option<DateTime<Utc>>,
    /// Their own limit on tool calls in one reply (an admin sets it); the
    /// shared default when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_rounds: Option<u32>,
}

/// Who a request comes from: what the app loop needs to know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Who {
    pub user: String,
    pub name: String,
    pub admin: bool,
}

impl Who {
    /// lyra itself (its own background work), with every right.
    pub fn server() -> Who {
        Who { user: OWNER.into(), name: "lyra".into(), admin: true }
    }
}

impl From<&User> for Who {
    fn from(u: &User) -> Who {
        Who { user: u.id.clone(), name: u.name.clone(), admin: u.role == Role::Admin }
    }
}

pub struct Users {
    path: PathBuf,
}

/// Who `key` is: an id, else an email, else a name nobody else has.
fn index(all: &[User], key: &str) -> Option<usize> {
    let key = key.trim();
    let k = key.to_lowercase();
    all.iter().position(|u| u.id == key).or_else(|| all.iter().position(|u| !u.email.is_empty() && u.email.to_lowercase() == k)).or_else(|| {
        let named: Vec<usize> = all.iter().enumerate().filter(|(_, u)| u.name.to_lowercase() == k).map(|(i, _)| i).collect();
        (named.len() == 1).then(|| named[0])
    })
}

impl Users {
    pub fn open(dir: &Path) -> Users {
        Users { path: dir.join("users.json") }
    }

    pub fn list(&self) -> Vec<User> {
        std::fs::read_to_string(&self.path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, users: &[User]) -> Result<(), String> {
        crate::devices::write_private(&self.path, &serde_json::to_string_pretty(users).map_err(|e| e.to_string())?)
    }

    pub fn get(&self, id: &str) -> Option<User> {
        self.list().into_iter().find(|u| u.id == id)
    }

    /// By id, then email, then a name only one person has (ignoring case).
    pub fn find(&self, key: &str) -> Option<User> {
        let all = self.list();
        index(&all, key).map(|i| all[i].clone())
    }

    /// The first user (an admin) when there are none yet. Returns whether one was made.
    pub fn ensure_owner(&self, name: &str) -> Result<bool, String> {
        let mut all = self.list();
        if !all.is_empty() {
            return Ok(false);
        }
        all.push(User { id: OWNER.into(), name: name.into(), email: String::new(), tenant: String::new(), oid: String::new(), role: Role::Admin, status: Status::Active, created: Utc::now(), last_seen: None, tool_rounds: None });
        self.save(&all).map(|_| true)
    }

    /// A new user (or the same one again: their details refreshed).
    pub fn upsert(&self, user: User) -> Result<User, String> {
        let mut all = self.list();
        match all.iter_mut().find(|u| u.id == user.id) {
            Some(u) => {
                u.name = user.name;
                u.email = user.email;
                u.tenant = user.tenant;
                let found = u.clone();
                self.save(&all)?;
                Ok(found)
            }
            None => {
                all.push(user.clone());
                self.save(&all)?;
                Ok(user)
            }
        }
    }

    /// Change someone: role and/or status. The last active admin stays one.
    pub fn update(&self, key: &str, role: Option<Role>, status: Option<Status>) -> Result<User, String> {
        let mut all = self.list();
        let i = index(&all, key).ok_or_else(|| format!("no user {key:?} (or more than one by that name: use their email)"))?;
        let admins = all.iter().filter(|u| u.role == Role::Admin && u.status == Status::Active).count();
        let losing = all[i].role == Role::Admin && all[i].status == Status::Active && (role == Some(Role::Member) || status.is_some_and(|s| s != Status::Active));
        if losing && admins <= 1 {
            return Err("that's the last admin: make someone else an admin first".into());
        }
        if let Some(r) = role {
            all[i].role = r;
        }
        if let Some(s) = status {
            all[i].status = s;
        }
        let u = all[i].clone();
        self.save(&all)?;
        Ok(u)
    }

    /// Set (or clear, with `None`) someone's own tool-call limit.
    pub fn set_tool_rounds(&self, key: &str, rounds: Option<u32>) -> Result<User, String> {
        let mut all = self.list();
        let i = index(&all, key).ok_or_else(|| format!("no user {key:?} (or more than one by that name: use their email)"))?;
        all[i].tool_rounds = rounds;
        let u = all[i].clone();
        self.save(&all)?;
        Ok(u)
    }

    /// Who a device acts as: its user, if they're active.
    pub fn who(&self, user: Option<&str>) -> Option<Who> {
        let u = self.get(user.unwrap_or(OWNER))?;
        (u.status == Status::Active).then(|| Who::from(&u))
    }

    /// Someone who signed in with Microsoft: the user they are (their details
    /// refreshed), the owner if it's the owner's email and the owner hasn't
    /// signed in yet, or a new member waiting for an admin.
    pub fn signed_in(&self, oid: &str, tenant: &str, name: &str, email: &str, owner_email: &str) -> Result<User, String> {
        let mut all = self.list();
        let is_owner_email = !owner_email.trim().is_empty() && email.eq_ignore_ascii_case(owner_email.trim());
        let i = match all.iter().position(|u| u.oid == oid) {
            Some(i) => i,
            None => match all.iter().position(|u| u.id == OWNER && u.oid.is_empty()).filter(|_| is_owner_email) {
                Some(i) => i,
                None => {
                    all.push(User { id: oid.into(), name: name.into(), email: email.into(), tenant: tenant.into(), oid: oid.into(), role: Role::Member, status: Status::Pending, created: Utc::now(), last_seen: None, tool_rounds: None });
                    all.len() - 1
                }
            },
        };
        let u = &mut all[i];
        u.oid = oid.into();
        u.tenant = tenant.into();
        if !name.is_empty() {
            u.name = name.into();
        }
        if !email.is_empty() {
            u.email = email.into();
        }
        u.last_seen = Some(Utc::now());
        let found = u.clone();
        self.save(&all)?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owner_comes_first_and_the_last_admin_stays() {
        let dir = std::env::temp_dir().join(format!("lyra-users-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let users = Users::open(&dir);
        assert!(users.who(None).is_none(), "no users yet");
        assert!(users.ensure_owner("Garrett").unwrap());
        assert!(!users.ensure_owner("again").unwrap());
        assert_eq!(users.who(None).unwrap(), Who { user: OWNER.into(), name: "Garrett".into(), admin: true });
        let dana = User { id: "oid-d".into(), name: "Dana".into(), email: "dana@fbcad.org".into(), tenant: "t".into(), oid: "oid-d".into(), role: Role::Member, status: Status::Pending, created: Utc::now(), last_seen: None, tool_rounds: None };
        users.upsert(dana).unwrap();
        assert!(users.who(Some("oid-d")).is_none(), "pending can't sign in");
        users.update("DANA@fbcad.org", None, Some(Status::Active)).unwrap();
        assert!(!users.who(Some("oid-d")).unwrap().admin);
        assert!(users.update("owner", Some(Role::Member), None).unwrap_err().contains("last admin"));
        users.update("dana", Some(Role::Admin), None).unwrap();
        users.update("owner", None, Some(Status::Disabled)).unwrap();
        assert!(users.who(None).is_none(), "disabled can't");
        users.update("owner", None, Some(Status::Active)).unwrap();
        // The owner's Microsoft sign-in claims the owner; anyone else waits.
        let stranger = users.signed_in("oid-x", "t", "Mallory", "mallory@fbcad.org", "garrett@fbcad.org").unwrap();
        assert_eq!((stranger.id.as_str(), stranger.status, stranger.role), ("oid-x", Status::Pending, Role::Member));
        let me = users.signed_in("oid-g", "t", "Garrett Post", "Garrett@fbcad.org", "garrett@fbcad.org").unwrap();
        assert_eq!((me.id.as_str(), me.oid.as_str(), me.role, me.name.as_str()), (OWNER, "oid-g", Role::Admin, "Garrett Post"), "the owner keeps their id");
        assert_eq!(users.signed_in("oid-g", "t", "Garrett Post", "garrett@fbcad.org", "garrett@fbcad.org").unwrap().id, OWNER, "and is found again");
        let again = users.signed_in("oid-y", "t", "Eve", "garrett@fbcad.org", "garrett@fbcad.org").unwrap();
        assert_eq!(again.status, Status::Pending, "the owner can only be claimed once");
    }
}
