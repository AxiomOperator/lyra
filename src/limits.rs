//! How many rounds of tool calls one reply may take: a person's own limit
//! (an admin sets it on the Users page or with `/users rounds`), else the
//! shared default (`max_tool_rounds` in `config/behavior.toml`, set on the
//! Settings page and tuned by evolution), else 8. A reply that reaches it
//! stops with a note and can be continued.

/// The most an admin may allow (one person or everyone).
pub const MAX_TOOL_ROUNDS: u32 = 64;

/// Without any setting.
pub const DEFAULT_TOOL_ROUNDS: u32 = 8;

/// The shared default: evolution's live value when it runs, else the file.
pub fn default_tool_rounds(evolved: Option<u32>) -> u32 {
    evolved
        .or_else(|| {
            let path = crate::config::dir()?.join("behavior.toml");
            let text = std::fs::read_to_string(path).ok()?;
            lyra_evolution::Behavior::parse(&text).ok().map(|b| b.max_tool_rounds)
        })
        .unwrap_or(DEFAULT_TOOL_ROUNDS)
        .clamp(1, MAX_TOOL_ROUNDS)
}

/// Someone's own limit, if an admin gave them one.
pub fn own_tool_rounds(user: &str) -> Option<u32> {
    let dir = crate::config::home()?.join("web");
    lyra_web::Users::open(&dir).get(user)?.tool_rounds
}

/// The limit for `user`'s replies.
pub fn tool_rounds(user: &str, evolved: Option<u32>) -> u32 {
    own_tool_rounds(user).map_or_else(|| default_tool_rounds(evolved), |n| n.clamp(1, MAX_TOOL_ROUNDS))
}

/// What a reply that reached its limit says: how far it got, and how to go on.
pub fn stopped_note(rounds: usize, own: bool, admin: bool) -> String {
    let whose = if own { "your limit" } else { "the limit" };
    let raise = if admin { " An admin can raise it: Settings (everyone) or Users (one person)." } else { "" };
    format!("Stopped after {rounds} rounds of tool calls ({whose}). The work so far is kept: press Continue, or say \"continue\", to keep going.{raise}")
}
