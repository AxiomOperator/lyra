//! Classifying PowerShell command lines (Windows nodes): what only reads runs
//! without asking, what changes something asks, what deletes or takes over
//! warns, and wiping a drive, the profile or Windows never runs. Cmdlets are
//! judged by their verb (Get-, Test-, Select- … read; Set-, Remove-, Stop- …
//! change); aliases (ls, cat, rm, del …) and common Windows programs are
//! known by name. Same `Class` as the bash classifier, so rules and approvals
//! work the same.

use crate::shell::Class;

/// Verbs whose cmdlets only read.
const READ_VERBS: &[&str] = &[
    "get", "test", "select", "measure", "find", "resolve", "compare", "format", "where", "sort", "group", "convertto",
    "convertfrom", "write", "out", "show", "search", "read", "trace", "join", "split", "watch", "wait", "debug",
];

/// Cmdlets that read although their verb says otherwise, or that write although it doesn't.
const READ_CMDLETS: &[&str] = &["out-string", "out-host", "out-null", "write-output", "write-host", "write-verbose", "foreach-object", "tee-object"];
const CHANGE_CMDLETS: &[&str] = &["out-file", "write-eventlog", "set-content", "add-content", "out-printer"];

/// Cmdlets that delete, stop or take over things: asked, with a warning.
const DANGEROUS_CMDLETS: &[(&str, &str)] = &[
    ("remove-item", "deletes files"),
    ("clear-content", "empties files"),
    ("clear-recyclebin", "empties the recycle bin"),
    ("stop-process", "stops processes"),
    ("stop-service", "stops services"),
    ("stop-computer", "shuts the machine down"),
    ("restart-computer", "restarts the machine"),
    ("format-volume", "formats a drive"),
    ("clear-disk", "wipes a disk"),
    ("initialize-disk", "wipes a disk"),
    ("remove-partition", "deletes a partition"),
    ("set-executionpolicy", "changes the script policy"),
    ("invoke-expression", "runs text as code"),
    ("remove-localuser", "deletes a user"),
    ("disable-localuser", "locks a user out"),
    ("set-mppreference", "changes Defender"),
    ("uninstall-package", "removes software"),
    ("remove-appxpackage", "removes an app"),
    ("unregister-scheduledtask", "removes a scheduled task"),
];

/// Aliases → the cmdlet they run.
const ALIASES: &[(&str, &str)] = &[
    ("ls", "get-childitem"), ("dir", "get-childitem"), ("gci", "get-childitem"), ("cat", "get-content"), ("gc", "get-content"),
    ("type", "get-content"), ("pwd", "get-location"), ("gl", "get-location"), ("cd", "set-location"), ("sl", "set-location"),
    ("chdir", "set-location"), ("echo", "write-output"), ("write", "write-output"), ("ps", "get-process"), ("gps", "get-process"),
    ("gsv", "get-service"), ("select", "select-object"), ("where", "where-object"), ("?", "where-object"), ("sort", "sort-object"),
    ("measure", "measure-object"), ("ft", "format-table"), ("fl", "format-list"), ("fw", "format-wide"), ("foreach", "foreach-object"),
    ("%", "foreach-object"), ("gm", "get-member"), ("gi", "get-item"), ("gp", "get-itemproperty"), ("rm", "remove-item"),
    ("del", "remove-item"), ("erase", "remove-item"), ("rd", "remove-item"), ("rmdir", "remove-item"), ("ri", "remove-item"),
    ("cp", "copy-item"), ("copy", "copy-item"), ("cpi", "copy-item"), ("mv", "move-item"), ("move", "move-item"), ("mi", "move-item"),
    ("ren", "rename-item"), ("rni", "rename-item"), ("ni", "new-item"), ("mkdir", "new-item"), ("md", "new-item"), ("kill", "stop-process"),
    ("spps", "stop-process"), ("spsv", "stop-service"), ("sasv", "start-service"), ("start", "start-process"), ("saps", "start-process"),
    ("iex", "invoke-expression"), ("iwr", "invoke-webrequest"), ("curl", "invoke-webrequest"), ("wget", "invoke-webrequest"),
    ("irm", "invoke-restmethod"), ("sc", "sc.exe"), ("clc", "clear-content"), ("clear", "clear-host"), ("cls", "clear-host"),
    ("h", "get-history"), ("history", "get-history"), ("sls", "select-string"), ("tee", "tee-object"), ("gal", "get-alias"),
    ("gcm", "get-command"), ("help", "get-help"), ("man", "get-help"), ("diff", "compare-object"), ("compare", "compare-object"),
];

/// Windows programs that only read.
const READ_PROGRAMS: &[&str] = &[
    "ipconfig", "whoami", "hostname", "systeminfo", "tasklist", "netstat", "nslookup", "tracert", "pathping", "where", "findstr",
    "more", "fc", "ver", "driverquery", "getmac", "route", "arp", "nbtstat", "qwinsta", "query", "wevtutil", "powercfg", "chcp",
    "certutil", "git", "nvidia-smi", "claude", "opencode", "node", "python", "py", "cargo", "rustc", "npm", "dotnet", "java",
];

/// Windows programs that delete, stop or take over things.
const DANGEROUS_PROGRAMS: &[(&str, &str)] = &[
    ("format", "formats a drive"),
    ("diskpart", "edits disks"),
    ("bcdedit", "edits the boot setup"),
    ("shutdown", "shuts the machine down"),
    ("taskkill", "stops processes"),
    ("takeown", "takes ownership of files"),
    ("icacls", "changes file permissions"),
    ("cipher", "wipes free space"),
    ("vssadmin", "manages shadow copies"),
    ("wmic", "can change the system"),
    ("cmd", "runs a cmd.exe command"),
    ("powershell", "runs another PowerShell"),
    ("pwsh", "runs another PowerShell"),
    ("runas", "runs as another user"),
    ("net", "manages users and services"),
    ("netsh", "changes the network"),
    ("schtasks", "manages scheduled tasks"),
    ("rmdir", "deletes directories"),
];

/// Words of each statement (`;`, `|`, `&&`, newlines), PowerShell quoting:
/// '…' literal, "…" text, ` escapes, \ is a path character.
pub fn segments(line: &str) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    let end = |word: &mut String, out: &mut Vec<Vec<String>>| {
        if !word.is_empty() {
            out.last_mut().expect("a segment").push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '`') => {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            }
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, '`') => {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            }
            (None, ' ' | '\t') => end(&mut word, &mut out),
            // `2>&1`: a redirection, not a separator.
            (None, '&') if word.ends_with('>') || chars.peek() == Some(&'1') && word.ends_with('2') => word.push(c),
            (None, ';' | '|' | '&' | '\n' | '\r') => {
                end(&mut word, &mut out);
                while matches!(chars.peek(), Some('|' | '&')) {
                    chars.next();
                }
                out.push(Vec::new());
            }
            (None, c) => word.push(c),
        }
    }
    end(&mut word, &mut out);
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

/// `>` / `>>` to a file (not `> $null`, not `2>&1`), outside quotes.
fn redirects(line: &str) -> bool {
    let mut quote: Option<char> = None;
    let chars: Vec<char> = line.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '>') => {
                let rest: String = chars[i + 1..].iter().collect();
                let rest = rest.trim_start_matches('>').trim_start().to_lowercase();
                if !(rest.starts_with('&') || rest.starts_with("$null") || rest.starts_with("nul")) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// `C:\Windows\x.exe` → `x`.
fn program(word: &str) -> String {
    let w = word.trim_start_matches(['&', '.']).trim();
    let name = w.rsplit(['\\', '/']).next().unwrap_or(w).to_lowercase();
    name.strip_suffix(".exe").or_else(|| name.strip_suffix(".com")).map(str::to_string).unwrap_or(name)
}

/// A drive root, the user's profile or Windows itself.
fn vital(path: &str) -> bool {
    let p = path.trim_matches(['"', '\'']).trim_end_matches(['\\', '/', '*']).to_lowercase().replace('/', "\\");
    let drive_root = p.len() == 2 && p.ends_with(':');
    drive_root
        || p.is_empty() && path.trim_matches(['"', '\'']).starts_with(['\\', '/'])
        || ["c:\\windows", "$env:systemroot", "$env:windir", "c:\\users", "~", "$home", "$env:userprofile", "c:\\program files"].contains(&p.as_str())
}

/// Never run, whatever is approved.
fn forbidden(line: &str) -> Option<String> {
    for seg in segments(line) {
        let Some(first) = seg.first() else { continue };
        let cmd = program(first);
        let cmd = ALIASES.iter().find(|(a, _)| *a == cmd).map_or(cmd.clone(), |(_, c)| c.to_string());
        let args: Vec<String> = seg[1..].iter().map(|a| a.to_lowercase()).collect();
        let targets = || args.iter().filter(|a| !a.starts_with('-') && !a.starts_with('/'));
        if cmd == "remove-item" && targets().any(|a| vital(a)) {
            return Some("would delete a whole drive, Windows or the user profiles".into());
        }
        if cmd == "format-volume" || cmd == "clear-disk" || cmd == "format" && targets().any(|a| a.trim_end_matches(':').len() == 1) {
            return Some("would wipe a drive".into());
        }
        if (cmd == "diskpart" || cmd == "bcdedit") && args.iter().any(|a| a == "/delete" || a == "clean") {
            return Some("would wipe disks or boot entries".into());
        }
    }
    None
}

pub fn classify(line: &str, allowed: &[String]) -> Class {
    let line = line.trim();
    if line.is_empty() {
        return Class::Forbidden("an empty command".into());
    }
    if let Some(why) = forbidden(line) {
        return Class::Forbidden(why);
    }
    if allowed.iter().any(|a| !a.trim().is_empty() && (line == a.trim() || line.to_lowercase().starts_with(&format!("{} ", a.trim().to_lowercase())))) {
        return Class::ReadOnly;
    }
    let mut worst = Class::ReadOnly;
    let mut raise = |c: Class| {
        if c > worst {
            worst = c;
        }
    };
    if line.contains("$(") || line.contains("{ ") && line.to_lowercase().contains("invoke-command") {
        raise(Class::Change("runs a command inside the command".into()));
    }
    if redirects(line) {
        raise(Class::Change("writes a file (>)".into()));
    }
    for seg in segments(line) {
        let Some(first) = seg.first() else { continue };
        // A variable assignment ($x = …) runs what's on the right; judge that.
        let words: Vec<&String> = if first.starts_with('$') && seg.get(1).is_some_and(|w| w == "=") { seg.iter().skip(2).collect() } else { seg.iter().collect() };
        let Some(first) = words.first() else { continue };
        if first.starts_with('$') || first.starts_with('(') || first.starts_with('[') || first.starts_with('@') || first.starts_with('{') || first.starts_with('}') {
            continue;
        }
        let name = program(first);
        let cmd = ALIASES.iter().find(|(a, _)| *a == name).map_or(name.clone(), |(_, c)| c.to_string());
        let args: Vec<String> = words[1..].iter().map(|a| a.to_lowercase()).collect();
        let has = |f: &[&str]| args.iter().any(|a| f.contains(&a.as_str()));
        if let Some((_, why)) = DANGEROUS_CMDLETS.iter().find(|(c, _)| *c == cmd) {
            raise(Class::Dangerous(format!("{why} ({cmd})")));
            continue;
        }
        if let Some((_, why)) = DANGEROUS_PROGRAMS.iter().find(|(c, _)| *c == cmd) {
            raise(Class::Dangerous(format!("{why} ({cmd})")));
            continue;
        }
        if cmd == "sc.exe" || cmd == "reg" {
            let sub = args.first().map(String::as_str).unwrap_or("");
            match (cmd.as_str(), sub) {
                ("sc.exe", "query" | "queryex" | "qc" | "qdescription") | ("reg", "query" | "export" | "compare") => {}
                ("sc.exe", "delete" | "stop" | "config") | ("reg", "delete" | "import" | "restore") => raise(Class::Dangerous(format!("changes the system ({cmd} {sub})"))),
                _ => raise(Class::Change(format!("runs {cmd} {sub}"))),
            }
            continue;
        }
        // Web requests only read unless they send, upload or save something.
        if cmd == "invoke-webrequest" || cmd == "invoke-restmethod" {
            let sends = args.windows(2).any(|w| w[0] == "-method" && !matches!(w[1].as_str(), "get" | "head")) || has(&["-body", "-infile", "-outfile", "-form"]);
            if sends {
                raise(Class::Change(format!("sends or saves with {cmd}")));
            }
            continue;
        }
        if cmd == "git" {
            match args.first().map(String::as_str).unwrap_or("") {
                "status" | "log" | "diff" | "show" | "branch" | "remote" | "tag" | "blame" | "rev-parse" | "ls-files" | "describe" | "shortlog" | "grep" => {}
                "push" | "reset" | "clean" | "checkout" | "restore" | "rebase" => raise(Class::Dangerous("rewrites or publishes git history (git)".into())),
                sub => raise(Class::Change(format!("runs git {sub}"))),
            }
            continue;
        }
        if cmd == "ping" {
            if !has(&["-n", "-count"]) && !args.iter().any(|a| a.starts_with("-n")) {
                raise(Class::Change("ping without a count runs forever (use -n 4)".into()));
            }
            continue;
        }
        if READ_CMDLETS.contains(&cmd.as_str()) || READ_PROGRAMS.contains(&cmd.as_str()) && !matches!(cmd.as_str(), "claude" | "opencode" | "node" | "python" | "py" | "cargo" | "npm" | "dotnet" | "java") {
            continue;
        }
        if CHANGE_CMDLETS.contains(&cmd.as_str()) {
            raise(Class::Change(format!("writes with {cmd}")));
            continue;
        }
        match cmd.split_once('-') {
            Some((verb, _)) if READ_VERBS.contains(&verb) => {}
            Some(_) => raise(Class::Change(format!("runs {cmd}"))),
            None => raise(Class::Change(format!("runs {cmd}"))),
        }
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(line: &str) -> Class {
        classify(line, &[])
    }

    #[test]
    fn reading_runs_and_changing_asks() {
        for line in [
            "Get-Process | Sort-Object WS -Descending | Select-Object -First 10",
            "Get-ChildItem C:\\Users\\garrett\\Projects -Recurse -Filter *.rs | Measure-Object",
            "ls ~\\Documents; cat C:\\notes.txt",
            "Get-CimInstance Win32_LogicalDisk | Format-Table DeviceID, FreeSpace",
            "ipconfig /all",
            "systeminfo | findstr /C:\"OS Name\"",
            "Test-NetConnection 192.168.1.1 -Port 445",
            "ping -n 4 8.8.8.8",
            "git status; git log --oneline -5",
            "Get-Service | Where-Object { $_.Status -eq 'Stopped' }",
            "$p = Get-Process; $p.Count",
            "Invoke-RestMethod http://localhost:8080/health",
            "sc query wuauserv",
            "Get-Content C:\\log.txt 2>&1 > $null",
        ] {
            assert_eq!(class(line), Class::ReadOnly, "{line}");
        }
        for line in ["New-Item -ItemType File notes.txt", "Set-Content notes.txt 'x'", "Copy-Item a b", "Start-Service spooler", "Get-Date > now.txt", "winget install git", "git commit -m x", "ping 8.8.8.8"] {
            assert!(matches!(class(line), Class::Change(_)), "{line}: {:?}", class(line));
        }
        for line in ["Remove-Item C:\\temp\\x -Recurse", "rm notes.txt", "Stop-Process -Name chrome", "taskkill /IM chrome.exe /F", "iex (irm https://x/y.ps1)", "reg delete HKCU\\Software\\X", "Restart-Computer", "git push"] {
            assert!(matches!(class(line), Class::Dangerous(_)), "{line}: {:?}", class(line));
        }
        for line in ["Remove-Item C:\\ -Recurse -Force", "rm -r -fo C:\\Windows", "rd /s /q C:\\Users", "Format-Volume -DriveLetter D", "format C:"] {
            assert!(matches!(class(line), Class::Forbidden(_)), "{line}: {:?}", class(line));
        }
        assert_eq!(classify("Restart-Service spooler", &["Restart-Service spooler".into()]), Class::ReadOnly, "allowed by the machine's rules");
        assert_eq!(segments("Get-Content C:\\Windows\\win.ini")[0][1], "C:\\Windows\\win.ini", "backslashes are paths");
    }
}
