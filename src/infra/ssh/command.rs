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

    if opts.batch_mode {
        args.push("-o".to_string());
        args.push("BatchMode=yes".to_string());
    }

    if let Some(timeout) = opts.connect_timeout {
        args.push("-o".to_string());
        args.push(format!("ConnectTimeout={}", timeout));
    }

    if opts.enable_control_master {
        if let Some(ref dir) = opts.control_path_dir {
            args.push("-o".to_string());
            args.push("ControlMaster=auto".to_string());
            args.push("-o".to_string());
            args.push(format!("ControlPath={}/cm/%C", dir));
            args.push("-o".to_string());
            args.push("ControlPersist=10m".to_string());
        }
    }

    for (key, val) in &host.ssh_options {
        args.push("-o".to_string());
        args.push(format!("{}={}", key, val));
    }

    args
}
