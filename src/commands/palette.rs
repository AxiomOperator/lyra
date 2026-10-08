//! The command palette: typing `/` lists the commands that match what's
//! typed so far, built from the same help text `/help` shows.

use std::sync::OnceLock;

/// One help line: how it's used, and what it does.
pub struct Entry {
    pub usage: String,
    pub description: String,
}

/// Every command, in `/help` order.
pub fn all() -> &'static [Entry] {
    static ALL: OnceLock<Vec<Entry>> = OnceLock::new();
    ALL.get_or_init(|| {
        let help = [crate::COMMANDS, crate::goals::COMMANDS, crate::agents::COMMANDS, crate::evolve::COMMANDS, crate::caps::COMMANDS, crate::HELP_END];
        parse(&help.join("\n"))
    })
}

fn parse(help: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for line in help.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // An indented line continues the previous description.
        if !line.starts_with('/') {
            if let Some(last) = out.last_mut() {
                last.description = format!("{} {}", last.description, line.trim()).trim().to_string();
            }
            continue;
        }
        let (usage, description) = match line.find("  ") {
            Some(i) => (&line[..i], line[i..].trim()),
            None => (line, ""),
        };
        out.push(Entry { usage: usage.trim().to_string(), description: description.to_string() });
    }
    out
}

/// The commands matching the input, or nothing when the input isn't a command.
pub fn matching(input: &str) -> Vec<&'static Entry> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    let query = input.trim_start().to_lowercase();
    let word = query.split_whitespace().next().unwrap_or("");
    let found: Vec<&Entry> = all().iter().filter(|e| e.usage.to_lowercase().starts_with(&query)).collect();
    if !found.is_empty() || !query.contains(' ') {
        return found;
    }
    // Typing arguments: keep showing that command's forms.
    all().iter().filter(|e| e.usage.split_whitespace().next() == Some(word)).collect()
}

/// What Tab fills in for an entry: the command and fixed subcommand, up to
/// the first placeholder (`<id>`, `[name]`) or alternative (`a|b` takes `a`).
pub fn completion(e: &Entry) -> String {
    let mut parts = Vec::new();
    let mut more = false;
    for w in e.usage.split_whitespace() {
        if w.starts_with('<') || w.starts_with('[') || w == "·" || w.starts_with('(') {
            more = true;
            break;
        }
        if let Some((first, _)) = w.split_once('|') {
            parts.push(first.to_string());
            more = true;
            break;
        }
        parts.push(w.to_string());
    }
    let mut text = parts.join(" ");
    if more {
        text.push(' ');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lines_become_entries() {
        let e = parse("/memory search <query>       recall with scores\n/plan run|resume [id]  execute it\n                             (pauses for approvals)\n/help  this list");
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].usage, "/memory search <query>");
        assert_eq!(e[1].description, "execute it (pauses for approvals)");
        assert_eq!(completion(&e[0]), "/memory search ");
        assert_eq!(completion(&e[1]), "/plan run ");
        assert_eq!(completion(&e[2]), "/help");
    }

    #[test]
    fn matches_narrow_as_you_type() {
        assert!(matching("hello").is_empty());
        assert!(matching("/").len() > 20, "a bare slash lists everything");
        assert!(matching("/mem").iter().all(|e| e.usage.starts_with("/mem")));
        let args = matching("/memory search tea");
        assert!(!args.is_empty() && args.iter().all(|e| e.usage.starts_with("/memory")));
        assert!(all().iter().any(|e| e.usage.starts_with("/agent new")));
    }
}
