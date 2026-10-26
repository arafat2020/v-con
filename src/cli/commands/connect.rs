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

/// Resolve `name`, record it, and replace the process with ssh.
pub fn run(
    bundle: &InventoryBundle,
    config: &Config,
    paths: &Paths,
    name: &str,
) -> Result<ConnectPlan, Error> {
    let program = system::find_ssh()?;
    let options = connect::session_options(config.multiplexing, &paths.control_dir());
    let plan = connect::prepare(&bundle.hosts, &paths.state_file(), &options, name)?;
    println!("{}", plan.banner);
    connect::replace_process(&program, &plan.args)?;
    Ok(plan)
}
