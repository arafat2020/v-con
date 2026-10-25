use tracing_subscriber::layer::Identity;

use crate::domain::host::EffectiveHost;

#[derive(Default)]
pub struct SshCommandOptions {
    pub batch_mode: bool,
    pub connect_timeout: Option<u64>,
    pub enable_control_master: bool,
    pub control_path_dir: Option<String>,
}

pub fn build_ssh_command(host: &EffectiveHost, opts: &SshCommandOptions) -> Vec<String> {
    let mut args = Vec::new();

    if host.port != 22 {
        args.push("-p".to_string());
        args.push(host.port.to_string());
    }

    if let Some(ref identity) = host.identity_file {
        args.push("-i".to_string());
        args.push(shellexpand::tilde(identity).to_string());
    }

    if let Some(ref jump) = host.jump {
        args.push("-J".to_string());
        args.push(jump.clone());
    }
}
