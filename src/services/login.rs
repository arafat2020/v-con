//! Obtain a login password, check it, and keep it in the keychain.
//!
//! vcon prompts on the terminal and hands the secret to ssh through an
//! askpass pipe. The password is stored only after `ssh` accepts it. A
//! rejected saved password is tried once, then replaced or reported.

use crate::domain::host::EffectiveHost;
use crate::error::Error;
use crate::infra::secrets::{self, AskpassMode, PasswordPrompt, SecretChannel, SecretStore};
use crate::infra::ssh::command::exec_arguments_auth;
use crate::services::exec::{FailureClass, classify_failure};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What ssh should use after a password has been resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAuth {
    /// Ask ssh for one password and skip batch mode.
    pub password_auth: bool,
    /// Askpass environment. Empty when no password is saved.
    pub env: Vec<(String, String)>,
}

/// Inputs for [`prepare_host_auth`].
pub struct Prepare<'a> {
    /// Keychain.
    pub store: &'a dyn SecretStore,
    /// Directory that holds the askpass helper.
    pub cache_dir: &'a Path,
    /// ssh binary.
    pub program: &'a Path,
    /// Host to authenticate.
    pub host: &'a EffectiveHost,
    /// Inventory, for jump resolution.
    pub hosts: &'a [EffectiveHost],
    /// A terminal is available for a prompt.
    pub interactive: bool,
    /// Start a ControlMaster so the following session can reuse it.
    pub multiplexing: bool,
    /// Control socket directory.
    pub control_dir: &'a Path,
}

/// Decide how to log in, prompting and verifying when a password is required.
///
/// A saved password is tried once. When it is rejected and `interactive` is
/// set, the user is prompted and the keychain is replaced only if the new
/// password works. Non-interactive rejection returns [`stale_password_message`].
pub fn prepare_host_auth(
    prep: &Prepare<'_>,
    prompt: &dyn PasswordPrompt,
) -> Result<SessionAuth, Error> {
    let stored = prep.store.contains(&prep.host.name).unwrap_or(false);
    if secrets::is_rejected(&prep.host.name) || (stored && !probe_stored(prep)?) {
        if !prep.interactive {
            return Err(Error::Message(secrets::stale_password_message(
                &prep.host.name,
            )));
        }
        return prompt_verify_store(prep, prompt);
    }
    if stored {
        return Ok(saved(prep, AskpassMode::Force));
    }
    if prep.interactive {
        match probe(prep, false, &[], None)? {
            Probe::Ok => return Ok(SessionAuth::batch()),
            Probe::AuthFailed => return prompt_verify_store(prep, prompt),
            Probe::Failed(error) => return Err(error),
        }
    }
    Ok(SessionAuth::batch())
}

/// Try `secret` against `host`. Nothing is written to the keychain.
pub fn password_works(
    program: &Path,
    host: &EffectiveHost,
    hosts: &[EffectiveHost],
    cache_dir: &Path,
    secret: &str,
    multiplexing: bool,
    control_dir: &Path,
) -> Result<(), Error> {
    match probe_with(&ProbeSpec {
        program,
        host,
        hosts,
        cache_dir,
        password_auth: true,
        env: &[],
        pipe_secret: Some(secret),
        multiplexing,
        control_dir,
    })? {
        Probe::Ok => Ok(()),
        Probe::AuthFailed => Err(Error::Message(
            "password was rejected; not saved".to_string(),
        )),
        Probe::Failed(error) => Err(error),
    }
}

impl SessionAuth {
    fn batch() -> Self {
        Self {
            password_auth: false,
            env: Vec::new(),
        }
    }
}

fn saved(prep: &Prepare<'_>, mode: AskpassMode) -> SessionAuth {
    let auth = secrets::saved_auth(prep.store, prep.cache_dir, prep.host, mode);
    SessionAuth {
        password_auth: auth.password_auth,
        env: auth.env,
    }
}

fn probe_stored(prep: &Prepare<'_>) -> Result<bool, Error> {
    let auth = secrets::saved_auth(prep.store, prep.cache_dir, prep.host, AskpassMode::Force);
    if !auth.password_auth {
        return Ok(false);
    }
    match probe(prep, true, &auth.env, None)? {
        Probe::Ok => Ok(true),
        Probe::AuthFailed => Ok(false),
        Probe::Failed(error) => Err(error),
    }
}

fn prompt_verify_store(
    prep: &Prepare<'_>,
    prompt: &dyn PasswordPrompt,
) -> Result<SessionAuth, Error> {
    let secret = prompt.ask(&prep.host.user, &prep.host.address)?;
    if secret.is_empty() {
        return Err(Error::Message("password prompt cancelled".to_string()));
    }
    match probe(prep, true, &[], Some(&secret))? {
        Probe::Ok => {
            prep.store.set(&prep.host.name, &secret)?;
            secrets::clear_rejection(&prep.host.name);
            Ok(saved(prep, AskpassMode::Force))
        }
        Probe::AuthFailed => Err(Error::Message(
            "password was rejected; not saved".to_string(),
        )),
        Probe::Failed(error) => Err(error),
    }
}

enum Probe {
    Ok,
    AuthFailed,
    Failed(Error),
}

struct ProbeSpec<'a> {
    program: &'a Path,
    host: &'a EffectiveHost,
    hosts: &'a [EffectiveHost],
    cache_dir: &'a Path,
    password_auth: bool,
    env: &'a [(String, String)],
    pipe_secret: Option<&'a str>,
    multiplexing: bool,
    control_dir: &'a Path,
}

fn probe(
    prep: &Prepare<'_>,
    password_auth: bool,
    env: &[(String, String)],
    pipe_secret: Option<&str>,
) -> Result<Probe, Error> {
    probe_with(&ProbeSpec {
        program: prep.program,
        host: prep.host,
        hosts: prep.hosts,
        cache_dir: prep.cache_dir,
        password_auth,
        env,
        pipe_secret,
        multiplexing: prep.multiplexing,
        control_dir: prep.control_dir,
    })
}

fn probe_with(spec: &ProbeSpec<'_>) -> Result<Probe, Error> {
    let ProbeSpec {
        program,
        host,
        hosts,
        cache_dir,
        password_auth,
        env,
        pipe_secret,
        multiplexing,
        control_dir,
    } = *spec;
    let control = multiplexing.then(|| control_dir.display().to_string());
    let args = exec_arguments_auth(
        host,
        hosts,
        host.connect_timeout.or(Some(15)),
        multiplexing,
        control.as_deref(),
        "true",
        password_auth,
    );
    let stored = pipe_secret.is_none() && secrets::uses_stored_password(env);
    let mut guard = match secrets::GateGuard::enter(&host.name, stored) {
        Ok(guard) => guard,
        Err(()) => return Ok(Probe::AuthFailed),
    };
    let mut command = Command::new(program);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut askpass_env = env.to_vec();
    if pipe_secret.is_some() {
        let helper = secrets::askpass_program(cache_dir)?;
        askpass_env = vec![
            ("SSH_ASKPASS".into(), helper.display().to_string()),
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
            ("VCON_ASKPASS_ACCOUNT".into(), host.name.clone()),
            ("VCON_ASKPASS_USER".into(), host.user.clone()),
            ("VCON_ASKPASS_ADDRESS".into(), host.address.clone()),
        ];
    }
    for (key, value) in &askpass_env {
        command.env(key, value);
    }
    let channel = match pipe_secret {
        Some(secret) => Some(SecretChannel::create(secret).map_err(|error| {
            Error::Message(format!("could not pass the password to ssh: {error}"))
        })?),
        None => None,
    };
    if let Some(channel) = &channel {
        command.env("VCON_ASKPASS_SOCK", channel.name());
    }
    crate::infra::ssh::system::release_dead_master(program, &args);
    let mut child = command
        .spawn()
        .map_err(|error| Error::Message(format!("failed to start ssh: {error}")))?;
    // Keep the channel alive until ssh has had a chance to call askpass.
    let _channel = channel;
    let status = wait_child(&mut child, Duration::from_secs(20))?;
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let code = status.and_then(|status| status.code());
    let class = classify_failure(code, &stderr);
    let auth_failed = stored && matches!(class, FailureClass::AuthFailed);
    guard.finish(auth_failed);
    match class {
        FailureClass::Ok => Ok(Probe::Ok),
        FailureClass::AuthFailed => Ok(Probe::AuthFailed),
        other => Ok(Probe::Failed(Error::Message(probe_error(
            &host.name, other, &stderr,
        )))),
    }
}

fn probe_error(host: &str, class: FailureClass, stderr: &str) -> String {
    let detail = stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    match class {
        FailureClass::ConnectionRefused => format!("{host}: connection refused"),
        FailureClass::TimedOut => format!("{host}: connection timed out"),
        FailureClass::HostKeyChanged => format!("{host}: host key changed"),
        FailureClass::Dns => format!("{host}: dns failure"),
        _ if detail.is_empty() => format!("{host}: ssh failed"),
        _ => format!("{host}: {detail}"),
    }
}

fn wait_child(
    child: &mut std::process::Child,
    limit: Duration,
) -> Result<Option<std::process::ExitStatus>, Error> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) if started.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Message("connection timed out".to_string()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(Error::Message(format!("ssh: {error}"))),
        }
    }
}

/// Read a password from `/dev/tty` without echoing it.
#[derive(Debug, Default)]
pub struct TtyPrompt;

impl PasswordPrompt for TtyPrompt {
    fn ask(&self, user: &str, address: &str) -> Result<String, Error> {
        prompt_from_tty(user, address)
    }
}

fn prompt_from_tty(user: &str, address: &str) -> Result<String, Error> {
    use std::io::Write;
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| Error::Message("no terminal for a password prompt".to_string()))?;
    let _restore = echo_off(&tty)?;
    write!(tty, "{user}@{address}'s password: ")
        .map_err(|error| Error::Message(error.to_string()))?;
    let _ = tty.flush();
    let mut line = String::new();
    let mut byte = [0u8; 1];
    loop {
        let n = tty
            .read(&mut byte)
            .map_err(|error| Error::Message(error.to_string()))?;
        if n == 0 || byte[0] == b'\n' {
            break;
        }
        if byte[0] == b'\r' {
            continue;
        }
        line.push(byte[0] as char);
    }
    writeln!(tty).ok();
    if line.is_empty() {
        return Err(Error::Message("password prompt cancelled".to_string()));
    }
    Ok(line)
}

struct EchoRestore {
    fd: i32,
    original: libc::termios,
}

fn echo_off(tty: &std::fs::File) -> Result<EchoRestore, Error> {
    use std::os::unix::io::AsRawFd;
    let fd = tty.as_raw_fd();
    let mut original: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
        return Err(Error::Message(
            "could not read terminal settings".to_string(),
        ));
    }
    let mut hidden = original;
    hidden.c_lflag |= libc::ICANON;
    hidden.c_lflag &= !libc::ECHO;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &hidden) } != 0 {
        return Err(Error::Message(
            "could not hide the password prompt".to_string(),
        ));
    }
    Ok(EchoRestore { fd, original })
}

impl Drop for EchoRestore {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}
