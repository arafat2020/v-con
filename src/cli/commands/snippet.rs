//! `vcon snippet list` and `vcon snippet run`.

use crate::config::Config;
use crate::domain::selector::{evaluate_selector, parse_selector};
use crate::domain::snippet;
use crate::error::Error;
use crate::paths::Paths;
use crate::services::inventory::InventoryBundle;
use crate::services::snippets::SnippetStore;
use std::collections::BTreeMap;
use std::process::ExitCode;

/// Run `vcon snippet list`.
pub fn list(paths: &Paths, json: bool) -> Result<ExitCode, Error> {
    let store = SnippetStore::load(&paths.snippets_file())?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(store.list())
                .map_err(|error| Error::Message(error.to_string()))?
        );
    } else if store.list().is_empty() {
        println!("(no snippets)");
    } else {
        for snippet in store.list() {
            let desc = snippet.description.as_deref().unwrap_or("").trim();
            if desc.is_empty() {
                println!("{:<16} {}", snippet.name, snippet.command);
            } else {
                println!("{:<16} {} — {}", snippet.name, desc, snippet.command);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Options for `vcon snippet run`.
pub struct RunRequest<'a> {
    /// Snippet name.
    pub name: &'a str,
    /// Host selector.
    pub selector: &'a str,
    /// `key=value` pairs from `--var`.
    pub vars: &'a [String],
    /// Skip confirmation.
    pub yes: bool,
    /// JSON exec output.
    pub json: bool,
}

/// Render a snippet and run it through the exec command.
pub fn run(
    bundle: &InventoryBundle,
    config: &Config,
    paths: &Paths,
    request: &RunRequest<'_>,
) -> Result<ExitCode, Error> {
    let store = SnippetStore::load(&paths.snippets_file())?;
    let snippet = store
        .get(request.name)
        .ok_or_else(|| Error::Message(format!("no snippet `{}`", request.name)))?
        .clone();
    let vars = parse_vars(request.vars)?;
    let command = snippet::render(&snippet.command, &vars).map_err(|missing| {
        Error::Message(format!(
            "missing variables for snippet `{}`: {}",
            snippet.name,
            missing.names.join(", ")
        ))
    })?;

    // `confirm = true` always asks unless `-y`.
    let yes = request.yes;
    if snippet.confirm && !yes {
        let terms = parse_selector(request.selector).map_err(Error::Message)?;
        let selection = evaluate_selector(&terms, &bundle.hosts);
        if let Some(error) = &selection.error {
            return Err(Error::Message(error.clone()));
        }
        let reason = format!(
            "snippet `{}` (confirm) on {} host(s)",
            snippet.name,
            selection.hosts.len()
        );
        eprint!("Run `{command}` — {reason}? [y/N] ");
        use std::io::Write;
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|error| Error::Message(error.to_string()))?;
        let answer = line.trim();
        if !(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")) {
            return Err(Error::Message("snippet cancelled".into()));
        }
    }

    let argv = vec![command.clone()];
    let code = crate::cli::commands::exec::run(
        bundle,
        config,
        paths,
        &crate::cli::commands::exec::ExecRequest {
            selector: request.selector,
            command: &argv,
            parallel: None,
            timeout: None,
            stream: false,
            grouped: false,
            only_failed: false,
            dry_run: false,
            sudo: false,
            json: request.json,
            // Snippet confirm already handled; `-y` still skips threshold/protected prompts.
            yes: request.yes || snippet.confirm,
            audit_action: Some("snippet"),
            audit_command: Some(format!("[{}] {command}", snippet.name)),
        },
    )?;
    Ok(code)
}

fn parse_vars(pairs: &[String]) -> Result<BTreeMap<String, String>, Error> {
    let mut vars = BTreeMap::new();
    for pair in pairs {
        let (key, value) = pair.split_once('=').ok_or_else(|| {
            Error::Message(format!("invalid --var `{pair}`; expected key=value"))
        })?;
        if key.is_empty() {
            return Err(Error::Message(format!(
                "invalid --var `{pair}`; empty key"
            )));
        }
        vars.insert(key.to_string(), value.to_string());
    }
    Ok(vars)
}
