use dialoguer::{Confirm, Input};
use indicatif::{ProgressBar, ProgressStyle};
use jiji_core::Result;
use owo_colors::{OwoColorize, Style};
use std::io::IsTerminal;
use std::time::Duration;

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

/// Clears the spinner when dropped.
pub struct SpinnerGuard {
    bar: ProgressBar,
}

impl SpinnerGuard {
    pub fn finish(&self, message: &str) {
        self.bar.finish_with_message(message.to_string());
    }
}

impl Drop for SpinnerGuard {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::format_duration;
    use std::time::Duration;

    #[test]
    fn durations_use_compact_precision() {
        assert_eq!(format_duration(Duration::from_millis(81)), "81ms");
        assert_eq!(format_duration(Duration::from_millis(1_240)), "1.2s");
        assert_eq!(format_duration(Duration::from_millis(12_840)), "12s");
    }
}
