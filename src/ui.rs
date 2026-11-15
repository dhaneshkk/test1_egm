//! Terminal output. Color only on a terminal and only without `NO_COLOR`.

use std::io::IsTerminal;
use std::sync::OnceLock;

fn color() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

pub fn paint(code: &str, s: &str) -> String {
    if color() { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
}

pub fn elephant(msg: &str) {
    println!("{} {msg}", paint("1;35", "🐘 elephant │"));
}

pub fn goldfish(msg: &str) {
    println!("{} {msg}", paint("1;33", "🐟 goldfish │"));
}

pub fn egm(msg: &str) {
    println!("{} {msg}", paint("1;36", "   egm-dsh  │"));
}

pub fn ok(msg: &str) {
    println!("{} {msg}", paint("1;32", "   ✔        │"));
}

pub fn warn(msg: &str) {
    eprintln!("{} {msg}", paint("1;31", "   ✖        │"));
}

pub fn tool(name: &str, detail: &str) {
    eprintln!("{}", paint("2", &format!("             ↳ {name} {detail}")));
}

/// Print a multi-line block indented under the speaker column.
pub fn block(text: &str) {
    for line in text.trim_matches('\n').trim_end().lines() {
        println!("             {line}");
    }
}

pub fn banner(title: &str) {
    println!("\n{}", paint("1;7", &format!(" {title} ")));
}
