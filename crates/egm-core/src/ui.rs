//! Terminal output, through the event sink (see [`crate::events`]). Color only on a terminal and
//! only without `NO_COLOR`.

use crate::events::{send, Event, Speaker};
use std::io::IsTerminal;
use std::sync::OnceLock;

fn color() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

pub fn paint(code: &str, s: &str) -> String {
    if color() { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
}

fn say(speaker: Speaker, text: impl Into<String>) {
    send(Event::Line { speaker, text: text.into() });
}

pub fn elephant(msg: &str) {
    say(Speaker::Elephant, msg);
}

pub fn goldfish(msg: &str) {
    say(Speaker::Goldfish, msg);
}

pub fn egm(msg: &str) {
    say(Speaker::Egm, msg);
}

pub fn ok(msg: &str) {
    say(Speaker::Ok, msg);
}

pub fn warn(msg: &str) {
    say(Speaker::Warn, msg);
}

pub fn tool(name: &str, detail: &str) {
    say(Speaker::Tool, format!("             ↳ {name} {detail}"));
}

/// Print a multi-line block indented under the speaker column.
pub fn block(text: &str) {
    say(Speaker::Block, text);
}

pub fn banner(title: &str) {
    say(Speaker::Banner, title);
}

/// Plain output (tables, JSON, a rendered document), one call per line or block.
pub fn out(text: impl Into<String>) {
    say(Speaker::Out, text);
}
