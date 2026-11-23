//! `vcon man`: print a troff man page to stdout.

use crate::cli::Cli;
use crate::error::Error;
use clap::CommandFactory;
use clap_mangen::Man;
use std::process::ExitCode;

/// Render the man page for the CLI definition.
pub fn run() -> Result<ExitCode, Error> {
    let page = render_page()?;
    print!("{page}");
    Ok(ExitCode::SUCCESS)
}

fn render_page() -> Result<String, Error> {
    let command = Cli::command();
    let man = Man::new(command);
    let mut buf = Vec::new();
    man.render(&mut buf)
        .map_err(|error| Error::Message(format!("could not write man page: {error}")))?;
    let text = String::from_utf8(buf)
        .map_err(|error| Error::Message(format!("man page was not utf-8: {error}")))?;
    Ok(strip_uninstalled_subpages(&text))
}

/// clap names each subcommand as its own manual (`vcon-init(1)`).
/// Those pages are not installed, so the reference is rewritten to the command.
pub fn strip_uninstalled_subpages(page: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in page.lines() {
        if line.trim() == ".SH SEE ALSO" {
            skipping = true;
            continue;
        }
        if skipping {
            if line.starts_with(".SH ") {
                skipping = false;
            } else {
                continue;
            }
        }
        out.push_str(&rewrite_manual_refs(line));
        out.push('\n');
    }
    out
}

/// `vcon\-init(1)` becomes `vcon init`. Nested dashes become spaces.
fn rewrite_manual_refs(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(index) = rest.find("vcon\\-") {
        out.push_str(&rest[..index]);
        out.push_str("vcon ");
        rest = &rest[index + "vcon\\-".len()..];
        if let Some(end) = rest.find("(1)") {
            out.push_str(&rest[..end].replace("\\-", " "));
            rest = &rest[end + "(1)".len()..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn see_also_subpages_are_removed() {
        let page = ".SH NAME\nvcon\n.SH SEE ALSO\n.BR vcon\\-init (1)\n.BR vcon\\-list (1)\n";
        let stripped = strip_uninstalled_subpages(page);
        assert!(stripped.contains(".SH NAME"));
        assert!(!stripped.contains("vcon\\-init"));
        assert!(!stripped.contains("SEE ALSO"));
        let rewritten = strip_uninstalled_subpages("vcon\\-init(1) and vcon\\-snippet\\-run(1)\n");
        assert_eq!(rewritten, "vcon init and vcon snippet run\n");
        assert!(!rewritten.contains("(1)"));
    }

    #[test]
    fn rendered_page_does_not_name_uninstalled_manuals() {
        let page = render_page().unwrap();
        assert!(!page.contains("vcon-init"));
        assert!(!page.contains("vcon\\-init"));
    }
}
