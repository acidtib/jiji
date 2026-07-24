use std::io::IsTerminal;

use anyhow::Context;
use jiji_config::{load_config, validate_config};
use jiji_network::NetworkPlanner;
use jiji_ssh::{PtyEvent, SshSession, StreamChunk};
use jiji_tui::Ui;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::ssh_adapter;

pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    command: Option<&str>,
    interactive: bool,
) -> anyhow::Result<()> {
    Ui::section("Server Exec:");

    if services.is_some() {
        anyhow::bail!(
            "`jiji server exec` does not accept -S/--services: a session targets exactly one host, not a service. Use -H/--hosts instead."
        );
    }

    let start = std::env::current_dir()?;
    let config_path = config_file.map(std::path::Path::new);
    let (config, path) = load_config(environment, config_path, &start)?;
    Ui::say(&format!("Configuration loaded from: {}", path.display()), 1);

    let validation = validate_config(&config);
    if !validation.valid {
        for error in validation.errors {
            Ui::error(&format!("{}: {}", error.path, error.message));
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }

    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running server exec.",
            path.display()
        )
    })?;

    let plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the private network plan: {error}"))?;
    let host_filters = split_comma_trimmed(hosts);
    let selected = plan.select_hosts(&host_filters)?;
    let target = match selected.as_slice() {
        [server] => server,
        [] => anyhow::bail!(
            "No server matched -H/--hosts. `jiji server exec` targets exactly one host; check the filter and try again."
        ),
        multiple => anyhow::bail!(
            "-H/--hosts matched {} servers ({}). `jiji server exec` targets exactly one host; narrow the filter and try again.",
            multiple.len(),
            multiple
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let named_server = config.servers.get(&target.name).ok_or_else(|| {
        anyhow::anyhow!(
            "Server '{}' selected by the network plan is not defined in configuration",
            target.name
        )
    })?;

    let options = ssh_adapter::connect_options(&target.name, named_server, &ssh)?;
    Ui::say(&format!("{}: connecting...", target.name), 1);
    let session = SshSession::connect(&options)
        .await
        .with_context(|| format!("Could not connect to '{}'", target.name))?;
    Ui::say(&format!("{}: connected", target.name), 1);

    let is_tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let want_pty = command.is_none() || interactive;

    if want_pty && !is_tty {
        if command.is_none() {
            session.close().await;
            anyhow::bail!(
                "An interactive shell requires a terminal. Run `jiji server exec` from an interactive session, or pass a command to run non-interactively."
            );
        }
        Ui::warn(
            "Not running in a terminal; ignoring --interactive and running non-interactively.",
        );
    }
    let effective_pty = want_pty && is_tty;

    let result = if effective_pty {
        run_pty(&session, command).await
    } else {
        // Reachable only with a command: the no-command, non-TTY case already bailed above.
        let command = command.expect("non-interactive path always has a command");
        run_streaming(&session, command).await
    };

    session.close().await;
    result
}

fn split_comma_trimmed(value: Option<&str>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

async fn run_streaming(session: &SshSession, command: &str) -> anyhow::Result<()> {
    let mut receiver = session.execute_streaming(command).await?;
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut exit_code = None;

    while let Some(item) = receiver.recv().await {
        match item? {
            StreamChunk::Stdout(data) => {
                stdout.write_all(&data).await?;
                stdout.flush().await?;
            }
            StreamChunk::Stderr(data) => {
                stderr.write_all(&data).await?;
                stderr.flush().await?;
            }
            StreamChunk::Exit(code) => exit_code = Some(code),
        }
    }

    match exit_code {
        Some(0) => Ok(()),
        Some(code) => anyhow::bail!("Remote command exited with status {code}"),
        None => anyhow::bail!(
            "Remote command did not report an exit status (connection closed or the command was terminated by a signal)"
        ),
    }
}

/// Drives an interactive PTY session: local raw mode, resize forwarding, and a bidirectional
/// relay between the local terminal and the remote channel. `command` is `None` for a login
/// shell, `Some` to attach a PTY to a specific command instead.
async fn run_pty(session: &SshSession, command: Option<&str>) -> anyhow::Result<()> {
    let (cols, rows) = terminal_size().context("Could not read the local terminal size")?;
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());
    let mut pty = session.open_pty(command, &term, cols, rows).await?;

    let _raw_mode = RawModeGuard::enable().context("Could not enable local raw terminal mode")?;
    let mut resize_signal =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
            .context("Could not register for terminal resize signals")?;

    let mut stdin_rx = spawn_stdin_reader();
    let mut stdout = tokio::io::stdout();
    let mut exit_code = None;

    loop {
        tokio::select! {
            chunk = stdin_rx.recv() => {
                match chunk {
                    Some(data) => {
                        let _ = pty.send(&data).await;
                    }
                    None => {
                        let _ = pty.eof().await;
                    }
                }
            }
            _ = resize_signal.recv() => {
                if let Ok((cols, rows)) = terminal_size() {
                    let _ = pty.resize(cols, rows).await;
                }
            }
            event = pty.recv() => {
                match event {
                    Some(PtyEvent::Output(data)) => {
                        stdout.write_all(&data).await?;
                        stdout.flush().await?;
                    }
                    Some(PtyEvent::Exit(code)) => exit_code = Some(code),
                    None => break,
                }
            }
        }
    }

    // Dropped explicitly (rather than left to end-of-scope) so the terminal is restored before
    // any further output -- including the error message below -- is written.
    drop(_raw_mode);

    match exit_code {
        Some(0) => Ok(()),
        Some(code) => anyhow::bail!("Remote command exited with status {code}"),
        None => anyhow::bail!(
            "Remote command did not report an exit status (connection closed or the command was terminated by a signal)"
        ),
    }
}

/// Reads stdin on a dedicated OS thread and forwards chunks through the returned channel, closing
/// it (dropping the sender) on EOF or a read error. Deliberately not `tokio::io::stdin()` polled
/// inside a `select!` loop: cancelling a `tokio::io::Stdin::read` future does not cancel the
/// underlying blocking read, and repeatedly creating/cancelling one every loop iteration leaves a
/// blocking-pool thread stuck waiting for input that will never come -- which then hangs process
/// exit, since (unlike a plain `std::thread`) the Tokio runtime waits for its blocking pool to
/// drain on shutdown.
fn spawn_stdin_reader() -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel(16);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = [0_u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

fn terminal_size() -> anyhow::Result<(u16, u16)> {
    let mut winsize: libc::winsize = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut winsize) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("ioctl(TIOCGWINSZ) failed on stdout");
    }
    let cols = if winsize.ws_col == 0 {
        80
    } else {
        winsize.ws_col
    };
    let rows = if winsize.ws_row == 0 {
        24
    } else {
        winsize.ws_row
    };
    Ok((cols, rows))
}

/// Puts the local terminal into raw mode for the guard's lifetime, restoring the original
/// settings on drop -- including on an early return, error propagation, or panic during
/// unwinding, so a killed connection or an unexpected error never leaves the user's terminal in
/// raw mode.
struct RawModeGuard {
    original: nix::sys::termios::Termios,
}

impl RawModeGuard {
    fn enable() -> anyhow::Result<Self> {
        let stdin = std::io::stdin();
        let original =
            nix::sys::termios::tcgetattr(&stdin).context("Could not read terminal settings")?;
        let mut raw = original.clone();
        nix::sys::termios::cfmakeraw(&mut raw);
        nix::sys::termios::tcsetattr(&stdin, nix::sys::termios::SetArg::TCSANOW, &raw)
            .context("Could not enable raw mode")?;
        Ok(Self { original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let stdin = std::io::stdin();
        let _ = nix::sys::termios::tcsetattr(
            &stdin,
            nix::sys::termios::SetArg::TCSANOW,
            &self.original,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comma_trimmed_hosts_split_the_same_way_as_other_commands() {
        assert_eq!(
            split_comma_trimmed(Some("app1, app2 ,app3")),
            vec!["app1", "app2", "app3"]
        );
        assert!(split_comma_trimmed(None).is_empty());
        assert!(split_comma_trimmed(Some("")).is_empty());
    }
}
