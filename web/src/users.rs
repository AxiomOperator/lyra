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
    pub role: Role,
    pub status: Status,
    pub created: DateTime<Utc>,
    #[serde(default)]
    pub last_seen: Option<DateTime<Utc>>,
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

    /// By id, email or name (ignoring case).
    pub fn find(&self, key: &str) -> Option<User> {
        let k = key.trim().to_lowercase();
        self.list().into_iter().find(|u| u.id == key.trim() || (!u.email.is_empty() && u.email.to_lowercase() == k) || u.name.to_lowercase() == k)
    }

    /// The first user (an admin) when there are none yet. Returns whether one was made.
    pub fn ensure_owner(&self, name: &str) -> Result<bool, String> {
        let mut all = self.list();
        if !all.is_empty() {
            return Ok(false);
        }
        all.push(User { id: OWNER.into(), name: name.into(), email: String::new(), tenant: String::new(), role: Role::Admin, status: Status::Active, created: Utc::now(), last_seen: None });
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
        let k = key.trim().to_lowercase();
        let i = all
            .iter()
            .position(|u| u.id == key.trim() || (!u.email.is_empty() && u.email.to_lowercase() == k) || u.name.to_lowercase() == k)
            .ok_or_else(|| format!("no user {key:?}"))?;
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

    /// Who a device acts as: its user, if they're active.
    pub fn who(&self, user: Option<&str>) -> Option<Who> {
        let u = self.get(user.unwrap_or(OWNER))?;
        (u.status == Status::Active).then(|| Who::from(&u))
    }

    /// A user's id changes when their Microsoft sign-in claims the owner placeholder.
    pub fn rename_id(&self, from: &str, to: &str) -> Result<(), String> {
        let mut all = self.list();
        if all.iter().any(|u| u.id == to) {
            return Err(format!("a user {to} exists already"));
        }
        let u = all.iter_mut().find(|u| u.id == from).ok_or_else(|| format!("no user {from}"))?;
        u.id = to.into();
        self.save(&all)
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
        let dana = User { id: "oid-d".into(), name: "Dana".into(), email: "dana@fbcad.org".into(), tenant: "t".into(), role: Role::Member, status: Status::Pending, created: Utc::now(), last_seen: None };
        users.upsert(dana).unwrap();
        assert!(users.who(Some("oid-d")).is_none(), "pending can't sign in");
        users.update("DANA@fbcad.org", None, Some(Status::Active)).unwrap();
        assert!(!users.who(Some("oid-d")).unwrap().admin);
        assert!(users.update("owner", Some(Role::Member), None).unwrap_err().contains("last admin"));
        users.update("dana", Some(Role::Admin), None).unwrap();
        users.update("owner", None, Some(Status::Disabled)).unwrap();
        assert!(users.who(None).is_none(), "disabled can't");
        users.rename_id("owner", "oid-g").unwrap();
        assert_eq!(users.get("oid-g").unwrap().name, "Garrett");
    }
}
