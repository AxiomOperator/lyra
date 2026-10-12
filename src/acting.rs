//! Whose behalf this thread works on: a conversation's turn, a routine's run,
//! a person's PMI follower. Per-person things (routines, goals, PMI) look it up
//! here; it's the owner unless set.

use std::cell::RefCell;

thread_local! {
    static USER: RefCell<String> = RefCell::new(lyra_web::users::OWNER.to_string());
    /// This thread works on a reply someone's waiting for (its own light
    /// calls, a page's summary, compacting, go ahead; lyra's background work
    /// waits for replies to finish).
    static REPLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Replies being written now (anywhere), for background work to wait on.
static REPLIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// While it's held, this thread is writing a reply.
pub struct Replying;

impl Replying {
    pub fn start() -> Self {
        REPLY.with(|r| r.set(true));
        REPLIES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Replying
    }
}

impl Drop for Replying {
    fn drop(&mut self) {
        REPLY.with(|r| r.set(false));
        REPLIES.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// This thread (or the one that started it) is writing a reply.
pub fn in_reply() -> bool {
    REPLY.with(|r| r.get())
}

/// How many replies are being written now.
pub fn replies() -> usize {
    REPLIES.load(std::sync::atomic::Ordering::SeqCst)
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
    let (user, job, reply) = (current(), crate::usage::job(), in_reply());
    std::thread::spawn(move || {
        set(&user);
        crate::usage::set_job(job);
        REPLY.with(|r| r.set(reply));
        f()
    })
}

/// `spawn` for a scoped thread (one that may borrow): the same person,
/// conversation and reply, for work done side by side within one step.
pub fn scoped<'scope, 'env, F, T>(s: &'scope std::thread::Scope<'scope, 'env>, f: F) -> std::thread::ScopedJoinHandle<'scope, T>
where
    F: FnOnce() -> T + Send + 'scope,
    T: Send + 'scope,
{
    let (user, job, reply) = (current(), crate::usage::job(), in_reply());
    s.spawn(move || {
        set(&user);
        crate::usage::set_job(job);
        REPLY.with(|r| r.set(reply));
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
