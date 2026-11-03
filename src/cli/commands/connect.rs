//! `vcon connect`: replace this process with an interactive ssh session.
//!
//! The attempt is written to the state file first. After `exec` there is no
//! vcon process left to update.

use crate::config::Config;
use crate::error::Error;
use crate::infra::secrets::{self, AskpassMode};
use crate::infra::ssh::system;
use crate::paths::Paths;
use crate::services::connect::{self, ConnectPlan};
use crate::services::inventory::InventoryBundle;

/// Resolve `name`, record it, and replace the process with ssh.
pub fn run(
    bundle: &InventoryBundle,
    config: &Config,
    paths: &Paths,
    name: &str,
) -> Result<ConnectPlan, Error> {
    let program = system::find_ssh()?;
    let host = connect::resolve_host(name, &bundle.hosts).map_err(Error::Message)?;
    let auth = secrets::saved_auth(
        secrets::system_store().as_ref(),
        &paths.cache_dir,
        host,
        AskpassMode::Prefer,
    );
    let mut options = connect::session_options(config.multiplexing, &paths.control_dir());
    options.password_auth = auth.password_auth;
    let mut plan = connect::prepare(&bundle.hosts, &paths.state_file(), &options, name)?;
    plan.askpass = auth.env;
    println!("{}", plan.banner);
    connect::replace_process(&program, &plan.args, &plan.askpass)?;
    Ok(plan)
}
