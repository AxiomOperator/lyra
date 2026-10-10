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

/// Start a background thread that keeps working for whoever this thread
/// works for (and counts its model calls for the same conversation). Every
/// background job starts this way: one started bare would quietly work as the
/// owner, with the owner's Microsoft account and PMI.
pub fn spawn<F, T>(f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (user, job) = (current(), crate::usage::job());
    std::thread::spawn(move || {
        set(&user);
        crate::usage::set_job(job);
        f()
    })
}

pub fn current() -> String {
    USER.with(|u| u.borrow().clone())
}

/// The owner (whose things stay in the main folders)?
pub fn is_owner(user: &str) -> bool {
    user == lyra_web::users::OWNER
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_background_job_keeps_whose_behalf_it_works_on() {
        let seen = super::run("dana", || super::spawn(super::current).join().unwrap());
        assert_eq!(seen, "dana", "not the owner by default");
        assert_eq!(super::current(), lyra_web::users::OWNER, "the spawning thread is back to the owner");
    }
}
