use dialoguer::{Confirm, Input};
use indicatif::{ProgressBar, ProgressStyle};
use jiji_core::Result;
use owo_colors::OwoColorize;
use std::time::Duration;

pub struct Ui;

impl Ui {
    /// Start a new section with clear visual separation: blank line + bold title.
    pub fn section(title: &str) {
        println!();
        println!("{}", title.bold());
    }

    /// Print a message indented by `indent` levels (2 spaces each).
    pub fn say(message: &str, indent: u8) {
        let indentation = "  ".repeat(indent as usize);
        println!("{indentation}{message}");
    }

    pub fn success(message: &str) {
        println!("{}", message.green());
    }

    pub fn warn(message: &str) {
        println!("{}", message.yellow());
    }

    pub fn error(message: &str) {
        eprintln!("{}", message.red());
    }

    /// Render a bordered panel with a bold title and a body.
    pub fn panel(title: &str, body: &str) {
        println!();
        println!("{}", title.bold());
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
