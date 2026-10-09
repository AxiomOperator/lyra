//! The people who use lyra (`~/.lyra/web/users.json`, readable by lyra's
//! user only): who they are, admin or member, and whether they may sign in.
//! People sign in with Microsoft, or with a username and password an admin
//! gave them (no Microsoft 365 needed: the password is kept as an Argon2
//! hash). Devices belong to a user; what a device may do comes from its user.
//! The first user is the owner, an admin; devices paired before users existed
//! are theirs.

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
    /// What they sign in with when there's no Microsoft account (lowercase).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    /// Their password, as an Argon2 hash (PHC string); empty: none set.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    /// The password was set by an admin (a first or reset one): they choose their own at sign-in.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub must_change: bool,
    /// A "forgot my password" link's token (its SHA-256: the link itself is only
    /// in their email) and until when it works.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_until: Option<DateTime<Utc>>,
}

/// How long a reset link works.
pub const RESET_FOR: chrono::Duration = chrono::Duration::minutes(30);

fn sha(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.trim().as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The fewest characters a password may have.
pub const MIN_PASSWORD: usize = 10;

fn hash(password: &str) -> Result<String, String> {
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
    let salt = SaltString::generate(&mut OsRng);
    argon2::Argon2::default().hash_password(password.as_bytes(), &salt).map(|h| h.to_string()).map_err(|e| e.to_string())
}

fn matches(hash: &str, password: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    PasswordHash::new(hash).is_ok_and(|h| argon2::Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// A password someone is given, to change at their first sign-in: 4 groups of 4, easy to read out.
pub fn temporary_password() -> String {
    let raw = crate::devices::random(16, b"abcdefghjkmnpqrstuvwxyz23456789");
    raw.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join("-")
}

/// A username: 3–32 lowercase letters, digits, dots, dashes or underscores.
pub fn valid_username(name: &str) -> Result<String, String> {
    let n = name.trim().to_lowercase();
    if n.len() < 3 || n.len() > 32 || !n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ".-_".contains(c)) || !n.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err("a username is 3 to 32 letters, digits, dots, dashes or underscores (like dana.doe)".into());
    }
    Ok(n)
}

fn check_strength(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(format!("a password needs at least {MIN_PASSWORD} characters"));
    }
    if password.chars().count() > 200 {
        return Err("that password is too long".into());
    }
    Ok(())
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

#[derive(Clone)]
pub struct Users {
    path: PathBuf,
}

/// The same, or a username.
fn index_or_username(all: &[User], key: &str) -> Option<usize> {
    index(all, key).or_else(|| {
        let k = key.trim().to_lowercase();
        all.iter().position(|u| !u.username.is_empty() && u.username == k)
    })
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
        all.push(User { id: OWNER.into(), name: name.into(), email: String::new(), tenant: String::new(), oid: String::new(), role: Role::Admin, status: Status::Active, created: Utc::now(), last_seen: None, tool_rounds: None, username: String::new(), password: String::new(), must_change: false, reset: String::new(), reset_until: None });
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

    /// "Forgot my password": a one-time token for someone who signs in with a
    /// password and has an email address (`None` otherwise: nobody is told
    /// which). Their old password keeps working until the link is used.
    pub fn start_reset(&self, username: &str) -> Result<Option<(User, String)>, String> {
        let key = username.trim().to_lowercase();
        let mut all = self.list();
        let Some(i) = all.iter().position(|u| !u.username.is_empty() && u.username == key) else { return Ok(None) };
        if all[i].status != Status::Active || !all[i].email.contains('@') {
            return Ok(None);
        }
        let token: String = (0..2).map(|_| uuid::Uuid::new_v4().simple().to_string()).collect();
        all[i].reset = sha(&token);
        all[i].reset_until = Some(Utc::now() + RESET_FOR);
        let u = all[i].clone();
        self.save(&all)?;
        Ok(Some((u, token)))
    }

    /// A reset link used: the new password set (checked), the link spent.
    pub fn finish_reset(&self, token: &str, new: &str) -> Result<User, String> {
        check_strength(new)?;
        let wanted = sha(token);
        let mut all = self.list();
        let i = all.iter().position(|u| !u.reset.is_empty() && u.reset == wanted).ok_or("that link has been used or replaced: ask for a new one")?;
        if all[i].reset_until.is_none_or(|t| t < Utc::now()) {
            all[i].reset.clear();
            all[i].reset_until = None;
            self.save(&all)?;
            return Err("that link has expired (they work for 30 minutes): ask for a new one".into());
        }
        if all[i].status != Status::Active {
            return Err("this account is turned off: ask an admin".into());
        }
        all[i].password = hash(new)?;
        all[i].must_change = false;
        all[i].reset.clear();
        all[i].reset_until = None;
        let u = all[i].clone();
        self.save(&all)?;
        Ok(u)
    }

    /// Set (or clear) someone's email address: where lyra's emails to them go.
    /// For accounts without Microsoft (whose sign-in brings its own address).
    pub fn set_email(&self, key: &str, email: &str) -> Result<User, String> {
        let email = email.trim();
        if !email.is_empty() && !(email.contains('@') && email.split('@').nth(1).is_some_and(|d| d.contains('.')) && !email.contains(char::is_whitespace)) {
            return Err(format!("{email:?} doesn't look like an email address"));
        }
        let mut all = self.list();
        let i = index_or_username(&all, key).ok_or_else(|| format!("no user {key:?} (or more than one by that name: use their email)"))?;
        if !email.is_empty() && all.iter().enumerate().any(|(j, u)| j != i && u.email.eq_ignore_ascii_case(email)) {
            return Err(format!("{email} is someone else's address here"));
        }
        all[i].email = email.to_string();
        let u = all[i].clone();
        self.save(&all)?;
        Ok(u)
    }

    /// A new account an admin makes for someone without Microsoft: active,
    /// with a one-time password (returned, shown to the admin once) to change
    /// at their first sign-in.
    pub fn create_local(&self, username: &str, name: &str, role: Role) -> Result<(User, String), String> {
        let username = valid_username(username)?;
        let name = name.trim();
        if name.is_empty() {
            return Err("give their name too (shown in lyra)".into());
        }
        let mut all = self.list();
        if all.iter().any(|u| u.username == username) {
            return Err(format!("someone already signs in as {username}"));
        }
        let temp = temporary_password();
        let id = format!("u-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let user = User { id, name: name.chars().take(80).collect(), email: String::new(), tenant: String::new(), oid: String::new(), role, status: Status::Active, created: Utc::now(), last_seen: None, tool_rounds: None, username, password: hash(&temp)?, must_change: true, reset: String::new(), reset_until: None };
        all.push(user.clone());
        self.save(&all)?;
        Ok((user, temp))
    }

    /// Give someone a username (and a one-time password): a Microsoft user, or
    /// the owner, who'd like to sign in without Microsoft too. Or reset a
    /// forgotten password. The one-time password comes back.
    pub fn reset_password(&self, key: &str, username: Option<&str>) -> Result<(User, String), String> {
        let mut all = self.list();
        let i = index_or_username(&all, key).ok_or_else(|| format!("no user {key:?} (or more than one by that name: use their email or username)"))?;
        if let Some(n) = username {
            let n = valid_username(n)?;
            if all.iter().enumerate().any(|(j, u)| j != i && u.username == n) {
                return Err(format!("someone already signs in as {n}"));
            }
            all[i].username = n;
        }
        if all[i].username.is_empty() {
            return Err(format!("{} has no username yet: give one (/users password {} <username>)", all[i].name, all[i].name));
        }
        let temp = temporary_password();
        all[i].password = hash(&temp)?;
        all[i].must_change = true;
        let u = all[i].clone();
        self.save(&all)?;
        Ok((u, temp))
    }

    /// Their own new password (the old one first, unless they're changing a one-time one just after signing in with it).
    pub fn change_password(&self, id: &str, old: Option<&str>, new: &str) -> Result<User, String> {
        check_strength(new)?;
        let mut all = self.list();
        let i = all.iter().position(|u| u.id == id).ok_or("no such user")?;
        if all[i].username.is_empty() {
            return Err("ask an admin for a username first (Users → Set password)".into());
        }
        if let Some(old) = old
            && !matches(&all[i].password, old)
        {
            return Err("that isn't your current password".into());
        }
        if matches(&all[i].password, new) {
            return Err("choose a different password from the one you have".into());
        }
        all[i].password = hash(new)?;
        all[i].must_change = false;
        let u = all[i].clone();
        self.save(&all)?;
        Ok(u)
    }

    /// Who signs in with this username and password (active ones only).
    pub fn sign_in(&self, username: &str, password: &str) -> Result<User, String> {
        let wrong = "that username and password don't match";
        let n = username.trim().to_lowercase();
        let mut all = self.list();
        let Some(i) = all.iter().position(|u| !u.username.is_empty() && u.username == n) else {
            // The same work either way: no telling which usernames exist by timing.
            static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
            let _ = matches(DUMMY.get_or_init(|| hash("not anyone's password").unwrap_or_default()), password);
            return Err(wrong.into());
        };
        if all[i].password.is_empty() || !matches(&all[i].password, password) {
            return Err(wrong.into());
        }
        match all[i].status {
            Status::Disabled => return Err("your lyra account is turned off: ask an admin".into()),
            Status::Pending => return Err("your account is waiting for an admin to let you in".into()),
            Status::Active => {}
        }
        all[i].last_seen = Some(Utc::now());
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
                    all.push(User { id: oid.into(), name: name.into(), email: email.into(), tenant: tenant.into(), oid: oid.into(), role: Role::Member, status: Status::Pending, created: Utc::now(), last_seen: None, tool_rounds: None, username: String::new(), password: String::new(), must_change: false, reset: String::new(), reset_until: None });
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
    fn a_reset_link_works_once_and_only_for_its_account() {
        let dir = std::env::temp_dir().join(format!("lyra-reset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let users = Users::open(&dir);
        users.ensure_owner("Owner").unwrap();
        let (dana, _) = users.create_local("dana", "Dana", Role::Member).unwrap();
        assert!(users.start_reset("dana").unwrap().is_none(), "no email: no link (and nothing says so)");
        users.set_email("dana", "dana@example.org").unwrap();
        assert!(users.start_reset("nobody").unwrap().is_none());
        let (_, token) = users.start_reset("Dana").unwrap().unwrap();
        assert!(!std::fs::read_to_string(dir.join("users.json")).unwrap().contains(&token), "only its hash is kept");
        assert!(users.finish_reset(&token, "short").unwrap_err().contains("10"), "the new one is checked");
        let u = users.finish_reset(&token, "a long new password").unwrap();
        assert_eq!(u.id, dana.id);
        assert!(users.sign_in("dana", "a long new password").is_ok());
        assert!(users.finish_reset(&token, "another long password").unwrap_err().contains("used"), "once");
    }

    #[test]
    fn local_accounts_sign_in_with_a_password_no_microsoft_needed() {
        let dir = std::env::temp_dir().join(format!("lyra-users-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let users = Users::open(&dir);
        users.ensure_owner("Owner").unwrap();
        let (dana, temp) = users.create_local("Dana.Doe", "Dana Doe", Role::Member).unwrap();
        assert_eq!((dana.username.as_str(), dana.status, dana.must_change), ("dana.doe", Status::Active, true));
        assert!(dana.id.starts_with("u-") && dana.oid.is_empty());
        assert_eq!(temp.len(), 19, "four groups of four: {temp}");
        assert!(!std::fs::read_to_string(dir.join("users.json")).unwrap().contains(&temp), "only the hash is kept");
        assert!(users.create_local("dana.doe", "Another Dana", Role::Member).unwrap_err().contains("already"));
        assert!(users.create_local("x", "X", Role::Member).is_err(), "too short");
        // Sign in with the one-time password, then choose one.
        assert_eq!(users.sign_in("DANA.DOE", &temp).unwrap().id, dana.id, "usernames ignore case");
        assert!(users.sign_in("dana.doe", "wrong password").is_err());
        assert!(users.sign_in("nobody", &temp).unwrap_err().contains("don't match"), "the same answer for an unknown name");
        assert!(users.change_password(&dana.id, None, "short").unwrap_err().contains("at least"));
        assert!(users.change_password(&dana.id, None, &temp).unwrap_err().contains("different"));
        let changed = users.change_password(&dana.id, None, "a long new passphrase").unwrap();
        assert!(!changed.must_change);
        assert!(users.sign_in("dana.doe", &temp).is_err() && users.sign_in("dana.doe", "a long new passphrase").is_ok());
        assert!(users.change_password(&dana.id, Some("not it"), "another long one").unwrap_err().contains("current"));
        // Forgot it: an admin resets it (another one-time password).
        let (_, again) = users.reset_password("Dana Doe", None).unwrap();
        assert!(users.sign_in("dana.doe", &again).unwrap().must_change);
        // The owner can have a username too; a turned-off account can't sign in.
        let (_, owner_temp) = users.reset_password(OWNER, Some("admin")).unwrap();
        assert!(users.sign_in("admin", &owner_temp).is_ok());
        users.update("dana.doe", None, Some(Status::Disabled)).unwrap_or_else(|_| users.update(&dana.id, None, Some(Status::Disabled)).unwrap());
        assert!(users.sign_in("dana.doe", &again).unwrap_err().contains("turned off"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_owner_comes_first_and_the_last_admin_stays() {
        let dir = std::env::temp_dir().join(format!("lyra-users-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let users = Users::open(&dir);
        assert!(users.who(None).is_none(), "no users yet");
        assert!(users.ensure_owner("Garrett").unwrap());
        assert!(!users.ensure_owner("again").unwrap());
        assert_eq!(users.who(None).unwrap(), Who { user: OWNER.into(), name: "Garrett".into(), admin: true });
        let dana = User { id: "oid-d".into(), name: "Dana".into(), email: "dana@fbcad.org".into(), tenant: "t".into(), oid: "oid-d".into(), role: Role::Member, status: Status::Pending, created: Utc::now(), last_seen: None, tool_rounds: None, username: String::new(), password: String::new(), must_change: false, reset: String::new(), reset_until: None };
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
