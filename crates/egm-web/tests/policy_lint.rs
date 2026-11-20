//! The phase 2 gate's policy test: every handler in `src/` checks the policy before it does
//! anything else.
//!
//! Topcoat reaches shards and procedures as their own POST endpoints that bypass page and
//! layout guards, so a check in a layout protects nothing. This test reads the source of every
//! `#[page]`, `#[route]`, `#[shard]` and `#[procedure]` and requires its first statement to be
//! a call into `authz`: `authz::require(..)` for anything under `/p/{project}`, at least
//! `authz::signed_in(..)` otherwise. The sign-in flow and the health check are the only
//! handlers allowed without one, listed by name below.

use std::path::Path;

const OPEN: &[&str] = &["sign_in", "auth_start", "auth_callback", "sign_out", "health"];
const HANDLER_ATTRS: &[&str] = &["#[page", "#[route", "#[shard", "#[procedure"];

#[derive(Debug)]
struct Handler {
    file: String,
    attr: String,
    name: String,
    first_statement: String,
}

/// Find each handler: its attribute line, its fn name, and the first statement of its body.
fn handlers(file: &str, src: &str) -> Vec<Handler> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = vec![];
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if !HANDLER_ATTRS.iter().any(|a| t.starts_with(a)) {
            continue;
        }
        // the fn line follows (possibly after more attributes)
        let Some(j) = (i + 1..lines.len()).find(|&j| lines[j].contains("fn ")) else { continue };
        let fn_line = lines[j];
        let name = fn_line.split("fn ").nth(1).and_then(|r| r.split(['(', '<']).next()).unwrap_or("").trim().to_string();
        // first non-empty line after the opening brace
        let first = (j + 1..lines.len()).map(|k| lines[k].trim()).find(|l| !l.is_empty() && !l.starts_with("//")).unwrap_or("").to_string();
        out.push(Handler { file: file.into(), attr: t.to_string(), name, first_statement: first });
    }
    out
}

fn violations(hs: &[Handler]) -> Vec<String> {
    let mut bad = vec![];
    for h in hs {
        if OPEN.contains(&h.name.as_str()) {
            continue;
        }
        let project_scoped = h.attr.contains("{project}");
        let ok = if project_scoped {
            // the project param is read first, then checked: allow `let p = project(cx);` before it
            is_check(&h.first_statement) || (h.first_statement.contains("project(cx)") && h.first_statement.starts_with("let "))
        } else {
            is_check(&h.first_statement) || h.first_statement.contains("authz::signed_in(")
        };
        if !ok {
            bad.push(format!("{}: `{}` ({}) does not start with a policy check: {}", h.file, h.name, h.attr, h.first_statement));
        }
    }
    bad
}

/// `authz::require(..)`, or `authz::require_feature(..)` (also bound: `let (v, st) = authz::require_feature(..)`).
fn is_check(l: &str) -> bool {
    let rhs = l.strip_prefix("let ").and_then(|r| r.split_once(" = ")).map(|(_, r)| r).unwrap_or(l);
    rhs.starts_with("authz::require(") || rhs.starts_with("authz::require_feature(")
}

/// For project-scoped handlers whose first line reads the path params, the check must be the
/// very next statement.
fn second_statement_checks(file: &str, src: &str) -> Vec<String> {
    let lines: Vec<&str> = src.lines().collect();
    let mut bad = vec![];
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if !(HANDLER_ATTRS.iter().any(|a| t.starts_with(a)) && t.contains("{project}")) {
            continue;
        }
        let Some(j) = (i + 1..lines.len()).find(|&j| lines[j].contains("fn ")) else { continue };
        let body: Vec<&str> = (j + 1..lines.len()).map(|k| lines[k].trim()).filter(|l| !l.is_empty() && !l.starts_with("//")).take(2).collect();
        if body.first().is_some_and(|l| l.starts_with("let ") && l.contains("project(cx)")) && !body.get(1).is_some_and(|l| is_check(l)) {
            bad.push(format!("{file}: handler at line {} reads its params but does not check the policy next", j + 1));
        }
    }
    bad
}

#[test]
fn every_handler_checks_the_policy_first() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut all = vec![];
    let mut bad = vec![];
    for e in std::fs::read_dir(&src_dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "rs") {
            let src = std::fs::read_to_string(&p).unwrap();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            all.extend(handlers(&name, &src));
            bad.extend(second_statement_checks(&name, &src));
        }
    }
    bad.extend(violations(&all));
    assert!(all.len() >= 12, "expected every handler to be found, found {}: {all:?}", all.len());
    for open in OPEN {
        assert!(all.iter().any(|h| h.name == *open), "listed as open but not a handler: {open}");
    }
    assert!(bad.is_empty(), "handlers without a policy check:\n{}", bad.join("\n"));
}

/// The lint must catch what it claims to: a handler without a check fails it.
#[test]
fn the_lint_catches_a_missing_check() {
    let src = r#"
#[page("/p/{project}/secret")]
async fn secret(cx: &Cx) -> Result<impl View> {
    let p = project(cx);
    let rows = db::features(&*app(cx).db, p).await.ah()?;
    Ok(view! { "x" })
}

#[shard]
async fn sneaky(cx: &Cx) -> Result<impl View> {
    Ok(view! { "y" })
}

#[route(POST "/p/{project}/f/{feature}/ok")]
async fn bound(cx: &Cx) -> Result<SeeOther> {
    let (p, slug) = (project(cx), feature(cx));
    let (v, st) = authz::require_feature(cx, p, slug, Action::Comment).await?;
    Ok(back())
}

#[route(POST "/p/{project}/f/{feature}/late")]
async fn late(cx: &Cx) -> Result<SeeOther> {
    let (p, slug) = (project(cx), feature(cx));
    let st = db::feature_state(&*app(cx).db, p, slug).await.ah()?;
    let (v, st) = authz::require_feature(cx, p, slug, Action::Comment).await?;
    Ok(back())
}

#[page("/p/{project}/fine")]
async fn fine(cx: &Cx) -> Result<impl View> {
    let p = project(cx);
    authz::require(cx, p, Action::Read).await?;
    Ok(view! { "z" })
}
"#;
    let hs = handlers("sample.rs", src);
    assert_eq!(hs.len(), 5);
    let mut bad = violations(&hs);
    bad.extend(second_statement_checks("sample.rs", src));
    assert!(bad.iter().any(|b| b.contains("sneaky")), "{bad:?}");
    assert!(bad.iter().any(|b| b.contains("does not check the policy next")), "{bad:?}");
    assert!(!bad.iter().any(|b| b.contains("`fine`") || b.contains("`bound`")), "{bad:?}");
    assert_eq!(bad.iter().filter(|b| b.contains("does not check the policy next")).count(), 2, "{bad:?}");
}
