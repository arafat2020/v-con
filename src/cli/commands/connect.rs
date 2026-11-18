//! `vcon connect`: replace this process with an interactive ssh session.
//!
//! The attempt is written to the state file first. After `exec` there is no
//! vcon process left to update.

use crate::config::Config;
use crate::error::Error;
use crate::infra::ssh::system;
use crate::paths::Paths;
use crate::services::connect::{self, ConnectPlan};
use crate::services::inventory::InventoryBundle;
use crate::services::login::{self, TtyPrompt};
use std::io::IsTerminal;

/// Resolve `name`, record it, and replace the process with ssh.
pub fn run(
    bundle: &InventoryBundle,
    config: &Config,
    paths: &Paths,
    name: &str,
) -> Result<ConnectPlan, Error> {
    let program = system::find_ssh()?;
    let host = connect::resolve_host(name, &bundle.hosts).map_err(Error::Message)?;
    let store = crate::infra::secrets::system_store();
    let ready = login::prepare_host_auth(
        &login::Prepare {
            store: store.as_ref(),
            cache_dir: &paths.cache_dir,
            program: &program,
            host,
            hosts: &bundle.hosts,
            interactive: std::io::stdin().is_terminal(),
            multiplexing: config.multiplexing,
            control_dir: &paths.control_dir(),
        },
        &TtyPrompt,
    )?;
    let mut options = connect::session_options(config.multiplexing, &paths.control_dir());
    options.password_auth = ready.password_auth;
    let mut plan = connect::prepare(&bundle.hosts, &paths.state_file(), &options, name)?;
    plan.askpass = ready.env;
    println!("{}", plan.banner);
    connect::replace_process(&program, &plan.args, &plan.askpass)?;
    Ok(plan)
}
