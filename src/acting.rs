//! Whose behalf this thread works on: a conversation's turn, a routine's run,
//! a person's PMI follower. Per-person things (routines, goals, PMI) look it up
//! here; it's the owner unless set.

use std::cell::RefCell;

thread_local! {
    static USER: RefCell<String> = RefCell::new(lyra_web::users::OWNER.to_string());
}

/// Work for this person from now on, on this thread.
pub fn set(user: &str) {
    USER.with(|u| *u.borrow_mut() = user.to_string());
}

/// Run `f` for this person, then go back to whoever it was.
pub fn run<T>(user: &str, f: impl FnOnce() -> T) -> T {
    let before = USER.with(|u| std::mem::replace(&mut *u.borrow_mut(), user.to_string()));
    let out = f();
    USER.with(|u| *u.borrow_mut() = before);
    out
}

pub fn current() -> String {
    USER.with(|u| u.borrow().clone())
}

/// The owner (whose things stay in the main folders)?
pub fn is_owner(user: &str) -> bool {
    user == lyra_web::users::OWNER
}
