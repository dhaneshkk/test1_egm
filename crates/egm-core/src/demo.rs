//! `egm-dsh demo`: the whole loop on the bundled `jot` sample, narrated.
//! With `--agent mock` it runs offline in seconds; with `--agent dsh` the same
//! script drives real DeepSeek Harness sessions.

use std::path::Path;

use crate::commands;
use crate::goldfish::Verdict;
use crate::project::{AgentKind, Config, Preset, Project};
use crate::ui;

const SAMPLE: [(&str, &str); 5] = [
    ("jot/__init__.py", ""),
    ("jot/cli.py", include_str!("../../../sample/before/jot/cli.py")),
    ("tests/__init__.py", ""),
    ("tests/test_jot.py", include_str!("../../../sample/before/tests/test_jot.py")),
    ("README.md", "# jot\n\nA tiny command-line notebook: `python3 -m jot.cli add \"text\"`, `python3 -m jot.cli list`.\nNotes live in `notes.json`. Tests: `python3 -m unittest discover -s tests -t .`\n"),
];

pub fn scaffold(dir: &Path) -> anyhow::Result<()> {
    for (rel, text) in SAMPLE {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, text)?;
    }
    Ok(())
}

pub fn run(dir: &Path, agent: AgentKind, preset: Preset, dsh_command: Option<Vec<String>>, triage: Option<crate::triage::TriageConfig>) -> anyhow::Result<()> {
    if dir.join(".egm-dsh").exists() {
        anyhow::bail!("{} already holds an egm-dsh project; pick an empty --dir", dir.display());
    }
    std::fs::create_dir_all(dir)?;
    scaffold(dir)?;
    let mut config = Config {
        agent,
        acceptance_cmd: ["python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q"].map(String::from).to_vec(),
        ..Config::default()
    };
    config.apply_preset(preset);
    config.triage = triage;
    if let Some(cmd) = dsh_command {
        config.dsh_command = cmd;
    }
    let p = Project::init(dir, config)?;
    crate::session::init(&Project::egm_dir(&p.root));
    let slug = "jot-tags";

    ui::banner(&format!("0 · Who runs what (preset: {preset:?})"));
    commands::roles(&p)?;

    ui::banner("1 · Feed the elephant: design before code (read-only)");
    commands::elephant(&p, slug, Some("Let people tag notes and list only the notes with a given tag."), None, &[])?;
    say(&p, slug, "Mostly me: I keep work and personal notes in one jot and want only the work ones during standup.")?;
    say(&p, slug, "Store tags on each note in notes.json as a list of strings. A note can have several tags; repeating a tag stores it once.")?;

    ui::banner("2 · Teach the elephant: write the design doc one section at a time");
    commands::draft(&p, slug, &[], false, None)?;

    ui::banner("3 · Test it against a goldfish");
    let mut verdict = commands::goldfish(&p, slug, false, true, false)?;

    if verdict != Verdict::Ready {
        ui::banner("4 · Fix the doc, not the chat: answer the goldfish and redraft");
        say(&p, slug, "Notes saved before this change have no tags field. Treat a missing field as an empty list and never rewrite old notes, so existing files need no migration.")?;
        commands::draft(&p, slug, &["Technical plan".into(), "Acceptance criteria".into()], false, None)?;
        ui::banner("5 · A brand-new goldfish reads the revised doc");
        verdict = commands::goldfish(&p, slug, false, true, false)?;
    }
    if verdict != Verdict::Ready {
        ui::warn("the goldfish still is not satisfied; the gate keeps `egm-dsh build` locked. Continue with `egm-dsh elephant jot-tags --say ...`.");
        return Ok(());
    }

    ui::banner("6 · Build with guardrails (workspace-write)");
    commands::build(&p, slug, None)?;

    ui::banner("7 · The mean review");
    commands::review(&p, slug)?;

    ui::banner("8 · Keep the doc true: drift check and status");
    commands::drift(&p)?;
    commands::label(&p, slug, "clean")?;
    commands::status(&p)?;
    commands::metrics(&p, false)?;

    ui::banner("9 · What it cost");
    commands::usage(&p, None)?;

    crate::ui::out("");
    ui::ok(&format!("done. Explore it: cd {} && egm-dsh status && egm-dsh show {slug}", dir.display()));
    Ok(())
}

fn say(p: &Project, slug: &str, msg: &str) -> anyhow::Result<()> {
    ui::egm(&format!("you: {msg}"));
    commands::elephant(p, slug, None, Some(msg), &[])
}
