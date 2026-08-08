use jiji_tui::Ui;

pub fn run() {
    Ui::say(
        &format!(
            "Jiji v{} {}",
            env!("CARGO_PKG_VERSION"),
            env!("JIJI_GIT_SHA")
        ),
        0,
    );
}
