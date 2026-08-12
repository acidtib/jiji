use dialoguer::{Confirm, Input};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use jiji_core::Result;
use owo_colors::{OwoColorize, Style};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

pub struct Ui;

impl Ui {
    /// Start a new section with clear visual separation.
    pub fn section(title: &str) {
        println!();
        let style = stdout_style(Style::new().bold().cyan());
        println!("{}", title.trim().style(style));
    }

    /// Print a message indented by `indent` levels (2 spaces each).
    ///
    /// Every line is indented independently so rendered summaries do not lose
    /// their hierarchy after the first newline.
    pub fn say(message: &str, indent: u8) {
        let indentation = "  ".repeat(indent as usize);
        for line in message.lines() {
            println!("{indentation}{line}");
        }
    }

    pub fn success(message: &str) {
        let style = stdout_style(Style::new().green().bold());
        println!("{} {}", "Done".style(style), message.trim_start());
    }

    pub fn success_elapsed(message: &str, elapsed: Duration) {
        let style = stdout_style(Style::new().green().bold());
        let timing = stdout_style(Style::new().dimmed());
        let message = message.trim_start().trim_end_matches('.');
        println!(
            "{} {} {}",
            "Done".style(style),
            message,
            format!("in {}", format_duration(elapsed)).style(timing)
        );
    }

    pub fn progress(title: &str, completed: usize, total: usize) {
        println!();
        let marker = stdout_style(Style::new().cyan().bold());
        let title_style = stdout_style(Style::new().bold());
        println!(
            "{} {} {completed}/{total}",
            "[+]".style(marker),
            title.trim().style(title_style)
        );
    }

    pub fn rule(width: usize, indent: u8) {
        let indentation = "  ".repeat(indent as usize);
        let style = stdout_style(Style::new().dimmed());
        println!("{indentation}{}", "-".repeat(width).style(style));
    }

    pub fn result_ok(label: &str, detail: &str) {
        result_line_stdout("OK", Style::new().green().bold(), label, detail);
    }

    pub fn result_warn(label: &str, detail: &str) {
        result_line_stdout("SKIP", Style::new().yellow().bold(), label, detail);
    }

    pub fn result_error(label: &str, detail: &str) {
        let status = "FAIL";
        eprintln!(
            "  {} {} {}",
            format!("{status:<4}").style(stderr_style(Style::new().red().bold())),
            label,
            detail
        );
    }

    pub fn warn(message: &str) {
        let style = stdout_style(Style::new().yellow().bold());
        println!("{} {}", "Warning:".style(style), message.trim_start());
    }

    pub fn error(message: &str) {
        let style = stderr_style(Style::new().red().bold());
        eprintln!("{} {}", "Error:".style(style), message.trim_start());
    }

    /// Render a bordered panel with a bold title and a body.
    pub fn panel(title: &str, body: &str) {
        println!();
        let style = stdout_style(Style::new().bold().cyan());
        println!("{}", title.trim().style(style));
        for line in body.lines() {
            println!("  {line}");
        }
    }

    pub fn confirm(prompt: &str, default: bool) -> Result<bool> {
        Confirm::new()
            .with_prompt(prompt)
            .default(default)
            .interact()
            .map_err(|e| jiji_core::JijiError::Other(e.to_string()))
    }

    /// Requires the user to type `expected` verbatim (exact match, no case/whitespace folding).
    /// For irreversible multi-host operations where a yes/no prompt is too easy to reflexively
    /// accept.
    pub fn confirm_typed(prompt: &str, expected: &str) -> Result<bool> {
        let input: String = Input::new()
            .with_prompt(prompt)
            .allow_empty(true)
            .interact_text()
            .map_err(|e| jiji_core::JijiError::Other(e.to_string()))?;
        Ok(input == expected)
    }

    pub fn spinner(message: &str) -> SpinnerGuard {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(Duration::from_millis(100));
        SpinnerGuard { bar }
    }

    /// Live, multi-endpoint deploy progress.
    ///
    /// Shows one overall counter plus one spinner row per endpoint.
    /// Falls back to plain line output when stderr is not a TTY (CI / piped).
    pub fn deploy_progress(identities: Vec<String>) -> DeployProgress {
        DeployProgress::new(identities)
    }

    /// Compatibility alias used by older call sites that still expect a single spinner
    /// for deploy-like work. Prefer `deploy_progress` for multi-endpoint runs.
    pub fn deploy_progress_with_servers(endpoints: Vec<(String, String)>) -> DeployProgress {
        DeployProgress::with_servers(endpoints)
    }

    pub fn server_setup_progress(hosts: Vec<String>) -> ServerSetupProgress {
        ServerSetupProgress::new(hosts)
    }

    pub fn proxy_restart_progress(hosts: Vec<String>) -> ServerSetupProgress {
        ServerSetupProgress::with_title(hosts, "Restarting proxy on".to_string())
    }
}

fn stdout_style(style: Style) -> Style {
    if std::io::stdout().is_terminal() {
        style
    } else {
        Style::new()
    }
}

fn stderr_style(style: Style) -> Style {
    if std::io::stderr().is_terminal() {
        style
    } else {
        Style::new()
    }
}

fn result_line_stdout(status: &str, style: Style, label: &str, detail: &str) {
    let status = format!("{status:<4}");
    println!(
        "  {} {} {}",
        status.style(stdout_style(style)),
        label,
        detail
    );
}

pub fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        format!("{millis}ms")
    } else if millis < 10_000 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        format!("{}s", duration.as_secs())
    }
}

/// Truncates to at most `max_bytes` bytes without splitting a UTF-8 char.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Clears the spinner when dropped.
pub struct SpinnerGuard {
    bar: ProgressBar,
}

impl SpinnerGuard {
    pub fn handle(&self) -> SpinnerHandle {
        SpinnerHandle {
            bar: self.bar.clone(),
        }
    }

    pub fn finish(&self, message: &str) {
        self.bar.finish_with_message(message.to_string());
    }
}

#[derive(Clone)]
pub struct SpinnerHandle {
    bar: ProgressBar,
}

impl SpinnerHandle {
    pub fn set_message(&self, message: &str) {
        self.bar.set_message(message.to_string());
    }
}

impl Drop for SpinnerGuard {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}

// ---------------------------------------------------------------------------
// Deploy live progress — MultiProgress dashboard
// ---------------------------------------------------------------------------

struct DeployProgressInner {
    multi: MultiProgress,
    overall: ProgressBar,
    bars: Mutex<HashMap<String, ProgressBar>>,
    starts: Mutex<HashMap<String, Instant>>,
    servers: HashMap<String, String>,
    total: usize,
    completed: AtomicUsize,
    succeeded: AtomicUsize,
    failed: AtomicUsize,
    skipped: AtomicUsize,
    start: Instant,
    tty: bool,
    title: String,
}

#[derive(Clone)]
pub struct DeployProgress {
    inner: Arc<DeployProgressInner>,
}

#[derive(Clone)]
pub struct DeployProgressHandle {
    inner: Arc<DeployProgressInner>,
}

impl DeployProgress {
    pub fn new(identities: Vec<String>) -> Self {
        Self::with_servers(
            identities
                .into_iter()
                .map(|id| (id, String::new()))
                .collect(),
        )
    }

    pub fn with_title(identities: Vec<String>, title: String) -> Self {
        Self::with_servers_and_title(
            identities
                .into_iter()
                .map(|id| (id, String::new()))
                .collect(),
            title,
        )
    }

    pub fn with_servers(endpoints: Vec<(String, String)>) -> Self {
        Self::with_servers_and_title(endpoints, "Deploying".to_string())
    }

    pub fn with_servers_and_title(endpoints: Vec<(String, String)>, title: String) -> Self {
        let tty = std::io::stderr().is_terminal();
        let total = endpoints.len();
        let start = Instant::now();

        let multi = MultiProgress::new();
        let overall = if tty && total > 0 {
            let bar = multi.add(ProgressBar::new(total as u64));
            let style = ProgressStyle::with_template(
                "{spinner:.cyan} {msg} {bar:22.cyan/dim} {pos}/{len} {elapsed_precise:.dim}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar());
            bar.set_style(style);
            bar.set_message(format!("{title} {} endpoint(s)", total));
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        } else {
            ProgressBar::hidden()
        };

        let mut bars_map = HashMap::new();
        let mut servers = HashMap::new();
        for (identity, server) in &endpoints {
            servers.insert(identity.clone(), server.clone());
            let bar = if tty {
                let pb = multi.add(ProgressBar::new_spinner());
                let style = ProgressStyle::with_template("{spinner:.cyan} {msg}")
                    .unwrap_or_else(|_| ProgressStyle::default_spinner());
                pb.set_style(style);
                let queued = if std::io::stderr().is_terminal() {
                    format!(
                        "{}  {}",
                        identity.style(stderr_style(Style::new().bold())),
                        "queued".style(stderr_style(Style::new().dimmed()))
                    )
                } else {
                    format!("{identity}  queued")
                };
                pb.set_message(queued);
                pb.enable_steady_tick(Duration::from_millis(100));
                pb
            } else {
                ProgressBar::hidden()
            };
            bars_map.insert(identity.clone(), bar);
        }

        if !tty && total > 0 {
            // Plain fallback header so piped/CI logs still show what is happening.
            let header = stdout_style(Style::new().bold().cyan());
            println!();
            println!(
                "{}",
                format!("{title} {} endpoint(s):", total).style(header)
            );
            for (identity, server) in &endpoints {
                if server.is_empty() {
                    println!("  - {identity}  queued");
                } else {
                    println!("  - {identity} @ {server}  queued");
                }
            }
        }

        let inner = DeployProgressInner {
            multi,
            overall,
            bars: Mutex::new(bars_map),
            starts: Mutex::new(HashMap::new()),
            servers,
            total,
            completed: AtomicUsize::new(0),
            succeeded: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
            start,
            tty,
            title,
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    pub fn handle(&self) -> DeployProgressHandle {
        DeployProgressHandle {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Clear the live dashboard before printing the final static summary.
    /// Idempotent; also called on Drop.
    pub fn finish(&self) {
        self.inner.finish_all();
    }

    pub fn total(&self) -> usize {
        self.inner.total
    }
}

impl DeployProgressHandle {
    pub fn set_status(&self, identity: &str, detail: &str) {
        // Record start time on first transition out of queued.
        {
            let mut starts = self.inner.starts.lock().unwrap();
            starts
                .entry(identity.to_string())
                .or_insert_with(Instant::now);
        }

        if self.inner.tty {
            if let Some(bar) = self.inner.bars.lock().unwrap().get(identity) {
                let server = self
                    .inner
                    .servers
                    .get(identity)
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" @ {s}"))
                    .unwrap_or_default();
                let msg = if std::io::stderr().is_terminal() {
                    let id_s = stderr_style(Style::new().bold());
                    let detail_s = stderr_style(Style::new().dimmed());
                    let id_str = format!("{identity}{server}");
                    let id = id_str.style(id_s);
                    let det = detail.style(detail_s);
                    format!("{id}  {det}")
                } else {
                    format!("{identity}{server}  {detail}")
                };
                bar.set_message(msg);
            }
        } else {
            let server = self
                .inner
                .servers
                .get(identity)
                .filter(|s| !s.is_empty())
                .map(|s| format!(" @ {s}"))
                .unwrap_or_default();
            // Throttled plain output: one line per state transition, not per poll.
            println!("  {identity}{server}: {detail}");
        }
    }

    pub fn mark_success(&self, identity: &str, detail: &str) {
        let elapsed = self
            .inner
            .starts
            .lock()
            .unwrap()
            .get(identity)
            .map(|s| s.elapsed())
            .unwrap_or_else(|| self.inner.start.elapsed());
        if self.inner.tty {
            if let Some(bar) = self.inner.bars.lock().unwrap().get(identity) {
                bar.set_style(
                    ProgressStyle::with_template("  {msg}")
                        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                );
                let server = self
                    .inner
                    .servers
                    .get(identity)
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" @ {s}"))
                    .unwrap_or_default();
                let msg = if std::io::stderr().is_terminal() {
                    let check_s = stderr_style(Style::new().green().bold());
                    let id_s = stderr_style(Style::new().bold());
                    let detail_s = stderr_style(Style::new().green());
                    let timing_s = stderr_style(Style::new().dimmed());
                    let check = "✔".style(check_s);
                    let id_str = format!("{identity}{server}");
                    let id = id_str.style(id_s);
                    let detail_styled = detail.style(detail_s);
                    let timing_str = format_duration(elapsed);
                    let timing = timing_str.style(timing_s);
                    format!("{check} {id}  {detail_styled}  {timing}")
                } else {
                    format!(
                        "✔ {identity}{server}  {detail}  {}",
                        format_duration(elapsed)
                    )
                };
                bar.finish_with_message(msg);
            }
        } else {
            let server = self
                .inner
                .servers
                .get(identity)
                .filter(|s| !s.is_empty())
                .map(|s| format!(" @ {s}"))
                .unwrap_or_default();
            println!(
                "  ✔ {identity}{server}  {detail}  {}",
                format_duration(elapsed)
            );
        }
        self.inner.completed.fetch_add(1, Ordering::SeqCst);
        self.inner.succeeded.fetch_add(1, Ordering::SeqCst);
        self.update_overall();
    }

    pub fn mark_failed(&self, identity: &str, error: &str) {
        let elapsed = self
            .inner
            .starts
            .lock()
            .unwrap()
            .get(identity)
            .map(|s| s.elapsed())
            .unwrap_or_else(|| self.inner.start.elapsed());
        // Keep error short for the live line; full error goes to final summary.
        let short_error = error.lines().next().unwrap_or(error).trim();
        let short_error = if short_error.len() > 80 {
            format!("{}…", truncate_at_char_boundary(short_error, 80))
        } else {
            short_error.to_string()
        };
        if self.inner.tty {
            if let Some(bar) = self.inner.bars.lock().unwrap().get(identity) {
                bar.set_style(
                    ProgressStyle::with_template("  {msg}")
                        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                );
                let server = self
                    .inner
                    .servers
                    .get(identity)
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" @ {s}"))
                    .unwrap_or_default();
                let msg = if std::io::stderr().is_terminal() {
                    let cross_s = stderr_style(Style::new().red().bold());
                    let id_s = stderr_style(Style::new().bold());
                    let timing_s = stderr_style(Style::new().dimmed());
                    let cross = "✘".style(cross_s);
                    let id_str = format!("{identity}{server}");
                    let id = id_str.style(id_s);
                    let timing_str = format_duration(elapsed);
                    let timing = timing_str.style(timing_s);
                    format!("{cross} {id}  failed: {short_error}  {timing}")
                } else {
                    format!(
                        "✘ {identity}{server}  failed: {short_error}  {}",
                        format_duration(elapsed)
                    )
                };
                bar.finish_with_message(msg);
            }
        } else {
            let server = self
                .inner
                .servers
                .get(identity)
                .filter(|s| !s.is_empty())
                .map(|s| format!(" @ {s}"))
                .unwrap_or_default();
            eprintln!(
                "  ✘ {identity}{server}  failed: {short_error}  {}",
                format_duration(elapsed)
            );
        }
        self.inner.completed.fetch_add(1, Ordering::SeqCst);
        self.inner.failed.fetch_add(1, Ordering::SeqCst);
        self.update_overall();
    }

    pub fn mark_skipped(&self, identity: &str) {
        if self.inner.tty {
            if let Some(bar) = self.inner.bars.lock().unwrap().get(identity) {
                bar.set_style(
                    ProgressStyle::with_template("  {msg}")
                        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                );
                let server = self
                    .inner
                    .servers
                    .get(identity)
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" @ {s}"))
                    .unwrap_or_default();
                let msg = if std::io::stderr().is_terminal() {
                    let mark_s = stderr_style(Style::new().yellow().bold());
                    let id_s = stderr_style(Style::new().bold());
                    let detail_s = stderr_style(Style::new().dimmed());
                    let mark = "○".style(mark_s);
                    let id_str = format!("{identity}{server}");
                    let id = id_str.style(id_s);
                    let detail_str = "skipped — sibling failed".to_string();
                    let detail = detail_str.style(detail_s);
                    format!("{mark} {id}  {detail}")
                } else {
                    format!("○ {identity}{server}  skipped — sibling failed")
                };
                bar.finish_with_message(msg);
            }
        } else {
            let server = self
                .inner
                .servers
                .get(identity)
                .filter(|s| !s.is_empty())
                .map(|s| format!(" @ {s}"))
                .unwrap_or_default();
            println!("  ○ {identity}{server}  skipped — sibling failed");
        }
        self.inner.completed.fetch_add(1, Ordering::SeqCst);
        self.inner.skipped.fetch_add(1, Ordering::SeqCst);
        self.update_overall();
    }

    pub fn finish_all(&self) {
        self.inner.finish_all();
    }

    fn update_overall(&self) {
        if !self.inner.tty {
            return;
        }
        let completed = self.inner.completed.load(Ordering::SeqCst);
        let succeeded = self.inner.succeeded.load(Ordering::SeqCst);
        let failed = self.inner.failed.load(Ordering::SeqCst);
        let skipped = self.inner.skipped.load(Ordering::SeqCst);
        let title = &self.inner.title;
        self.inner.overall.set_position(completed as u64);
        let msg = if failed > 0 || skipped > 0 {
            format!(
                "{title} {}/{}  {} ok, {} failed{}",
                completed,
                self.inner.total,
                succeeded,
                failed,
                if skipped > 0 {
                    format!(", {skipped} skipped")
                } else {
                    String::new()
                }
            )
        } else {
            format!("{title} {}/{}", completed, self.inner.total)
        };
        self.inner.overall.set_message(msg);
        if completed >= self.inner.total {
            self.inner.overall.finish_with_message(format!(
                "{title} {}/{}  done in {}",
                completed,
                self.inner.total,
                format_duration(self.inner.start.elapsed())
            ));
        }
    }
}

impl DeployProgressInner {
    fn finish_all(&self) {
        if self.tty {
            // Clear per-endpoint spinners that never completed (e.g. early bail)
            // so the final static summary does not interleave with live bars.
            let bars = self.bars.lock().unwrap();
            for bar in bars.values() {
                if !bar.is_finished() {
                    bar.finish_and_clear();
                }
            }
            if !self.overall.is_finished() {
                self.overall.finish_and_clear();
            }
            // MultiProgress does not need explicit clear; dropping bars hides them.
            let _ = &self.multi;
        }
    }
}

impl Drop for DeployProgress {
    fn drop(&mut self) {
        self.inner.finish_all();
    }
}

// ---------------------------------------------------------------------------
// Server setup live progress — MultiProgress per-host dashboard
// ---------------------------------------------------------------------------

struct ServerSetupProgressInner {
    multi: MultiProgress,
    overall: ProgressBar,
    bars: Mutex<HashMap<String, ProgressBar>>,
    starts: Mutex<HashMap<String, Instant>>,
    total: usize,
    completed: AtomicUsize,
    succeeded: AtomicUsize,
    failed: AtomicUsize,
    start: Instant,
    tty: bool,
    title: String,
}

#[derive(Clone)]
pub struct ServerSetupProgress {
    inner: Arc<ServerSetupProgressInner>,
}

#[derive(Clone)]
pub struct ServerSetupProgressHandle {
    inner: Arc<ServerSetupProgressInner>,
}

impl ServerSetupProgress {
    pub fn new(hosts: Vec<String>) -> Self {
        Self::with_title(hosts, "Setting up".to_string())
    }

    pub fn with_title(hosts: Vec<String>, title: String) -> Self {
        let tty = std::io::stderr().is_terminal();
        let total = hosts.len();
        let start = Instant::now();

        let multi = MultiProgress::new();
        let overall = if tty && total > 0 {
            let bar = multi.add(ProgressBar::new(total as u64));
            let style = ProgressStyle::with_template(
                "{spinner:.cyan} {msg} {bar:22.cyan/dim} {pos}/{len} {elapsed_precise:.dim}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar());
            bar.set_style(style);
            bar.set_message(format!("{title} {} server(s)", total));
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        } else {
            ProgressBar::hidden()
        };

        let mut bars_map = HashMap::new();
        for host in &hosts {
            let bar = if tty {
                let pb = multi.add(ProgressBar::new_spinner());
                let style = ProgressStyle::with_template("{spinner:.cyan} {msg}")
                    .unwrap_or_else(|_| ProgressStyle::default_spinner());
                pb.set_style(style);
                let queued = if std::io::stderr().is_terminal() {
                    format!(
                        "{}  {}",
                        host.style(stderr_style(Style::new().bold())),
                        "queued".style(stderr_style(Style::new().dimmed()))
                    )
                } else {
                    format!("{host}  queued")
                };
                pb.set_message(queued);
                pb.enable_steady_tick(Duration::from_millis(100));
                pb
            } else {
                ProgressBar::hidden()
            };
            bars_map.insert(host.clone(), bar);
        }

        if !tty && total > 0 {
            let header = stdout_style(Style::new().bold().cyan());
            println!();
            println!("{}", format!("{title} {} server(s):", total).style(header));
            for host in &hosts {
                println!("  - {host}  queued");
            }
        }

        let inner = ServerSetupProgressInner {
            multi,
            overall,
            bars: Mutex::new(bars_map),
            starts: Mutex::new(HashMap::new()),
            total,
            completed: AtomicUsize::new(0),
            succeeded: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
            start,
            tty,
            title: title.clone(),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    pub fn handle(&self) -> ServerSetupProgressHandle {
        ServerSetupProgressHandle {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn finish(&self) {
        self.inner.finish_all();
    }

    pub fn total(&self) -> usize {
        self.inner.total
    }
}

impl ServerSetupProgressHandle {
    pub fn set_status(&self, host: &str, detail: &str) {
        {
            let mut starts = self.inner.starts.lock().unwrap();
            starts.entry(host.to_string()).or_insert_with(Instant::now);
        }
        if self.inner.tty {
            if let Some(bar) = self.inner.bars.lock().unwrap().get(host) {
                let msg = if std::io::stderr().is_terminal() {
                    let host_s = stderr_style(Style::new().bold());
                    let detail_s = stderr_style(Style::new().dimmed());
                    let h = host.style(host_s);
                    let d = detail.style(detail_s);
                    format!("{h}  {d}")
                } else {
                    format!("{host}  {detail}")
                };
                bar.set_message(msg);
            }
        } else {
            println!("  {host}: {detail}");
        }
    }

    pub fn mark_success(&self, host: &str, detail: &str) {
        let elapsed = self
            .inner
            .starts
            .lock()
            .unwrap()
            .get(host)
            .map(|s| s.elapsed())
            .unwrap_or_else(|| self.inner.start.elapsed());
        if self.inner.tty {
            if let Some(bar) = self.inner.bars.lock().unwrap().get(host) {
                bar.set_style(
                    ProgressStyle::with_template("  {msg}")
                        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                );
                let msg = if std::io::stderr().is_terminal() {
                    let check_s = stderr_style(Style::new().green().bold());
                    let host_s = stderr_style(Style::new().bold());
                    let detail_s = stderr_style(Style::new().green());
                    let timing_s = stderr_style(Style::new().dimmed());
                    let check = "✔".style(check_s);
                    let h = host.style(host_s);
                    let d = detail.style(detail_s);
                    let timing_str = format_duration(elapsed);
                    let t = timing_str.style(timing_s);
                    format!("{check} {h}  {d}  {t}")
                } else {
                    format!("✔ {host}  {detail}  {}", format_duration(elapsed))
                };
                bar.finish_with_message(msg);
            }
        } else {
            println!("  ✔ {host}  {detail}  {}", format_duration(elapsed));
        }
        self.inner.completed.fetch_add(1, Ordering::SeqCst);
        self.inner.succeeded.fetch_add(1, Ordering::SeqCst);
        self.update_overall();
    }

    pub fn mark_failed(&self, host: &str, error: &str) {
        let elapsed = self
            .inner
            .starts
            .lock()
            .unwrap()
            .get(host)
            .map(|s| s.elapsed())
            .unwrap_or_else(|| self.inner.start.elapsed());
        let short_error = error.lines().next().unwrap_or(error).trim();
        let short_error = if short_error.len() > 80 {
            format!("{}…", truncate_at_char_boundary(short_error, 80))
        } else {
            short_error.to_string()
        };
        if self.inner.tty {
            if let Some(bar) = self.inner.bars.lock().unwrap().get(host) {
                bar.set_style(
                    ProgressStyle::with_template("  {msg}")
                        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                );
                let msg = if std::io::stderr().is_terminal() {
                    let cross_s = stderr_style(Style::new().red().bold());
                    let host_s = stderr_style(Style::new().bold());
                    let timing_s = stderr_style(Style::new().dimmed());
                    let cross = "✘".style(cross_s);
                    let h = host.style(host_s);
                    let timing_str = format_duration(elapsed);
                    let t = timing_str.style(timing_s);
                    format!("{cross} {h}  failed: {short_error}  {t}")
                } else {
                    format!(
                        "✘ {host}  failed: {short_error}  {}",
                        format_duration(elapsed)
                    )
                };
                bar.finish_with_message(msg);
            }
        } else {
            eprintln!(
                "  ✘ {host}  failed: {short_error}  {}",
                format_duration(elapsed)
            );
        }
        self.inner.completed.fetch_add(1, Ordering::SeqCst);
        self.inner.failed.fetch_add(1, Ordering::SeqCst);
        self.update_overall();
    }

    pub fn finish_all(&self) {
        self.inner.finish_all();
    }

    fn update_overall(&self) {
        if !self.inner.tty {
            return;
        }
        let completed = self.inner.completed.load(Ordering::SeqCst);
        let succeeded = self.inner.succeeded.load(Ordering::SeqCst);
        let failed = self.inner.failed.load(Ordering::SeqCst);
        self.inner.overall.set_position(completed as u64);
        let title = &self.inner.title;
        let msg = if failed > 0 {
            format!(
                "{title} {}/{}  {} ok, {} failed",
                completed, self.inner.total, succeeded, failed
            )
        } else {
            format!("{title} {}/{}", completed, self.inner.total)
        };
        self.inner.overall.set_message(msg);
        if completed >= self.inner.total {
            self.inner.overall.finish_with_message(format!(
                "{title} {}/{}  done in {}",
                completed,
                self.inner.total,
                format_duration(self.inner.start.elapsed())
            ));
        }
    }
}

impl ServerSetupProgressInner {
    fn finish_all(&self) {
        if self.tty {
            let bars = self.bars.lock().unwrap();
            for bar in bars.values() {
                if !bar.is_finished() {
                    bar.finish_and_clear();
                }
            }
            if !self.overall.is_finished() {
                self.overall.finish_and_clear();
            }
            let _ = &self.multi;
        }
    }
}

impl Drop for ServerSetupProgress {
    fn drop(&mut self) {
        self.inner.finish_all();
    }
}

#[cfg(test)]
mod tests {
    use super::{format_duration, SpinnerHandle};
    use indicatif::ProgressBar;
    use std::time::Duration;

    #[test]
    fn durations_use_compact_precision() {
        assert_eq!(format_duration(Duration::from_millis(81)), "81ms");
        assert_eq!(format_duration(Duration::from_millis(1_240)), "1.2s");
        assert_eq!(format_duration(Duration::from_millis(12_840)), "12s");
    }

    #[test]
    fn spinner_handle_updates_the_shared_message() {
        let bar = ProgressBar::hidden();
        let handle = SpinnerHandle { bar: bar.clone() };

        handle.set_message("waiting for health check");

        assert_eq!(bar.message(), "waiting for health check");
    }

    #[test]
    fn deploy_progress_tracks_success_and_failure_counts() {
        let progress = super::DeployProgress::new(vec!["a".to_string(), "b".to_string()]);
        let handle = progress.handle();
        handle.set_status("a", "starting");
        handle.mark_success("a", "abc123def456789");
        handle.mark_failed("b", "boom");
        assert_eq!(
            progress
                .inner
                .completed
                .load(std::sync::atomic::Ordering::SeqCst),
            2
        );
        assert_eq!(
            progress
                .inner
                .succeeded
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            progress
                .inner
                .failed
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[test]
    fn deploy_progress_handles_skipped() {
        let progress = super::DeployProgress::new(vec!["a".to_string()]);
        let handle = progress.handle();
        handle.mark_skipped("a");
        assert_eq!(
            progress
                .inner
                .skipped
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }
}
