use jiji_tui::Ui;

pub fn run() {
    Ui::section("Version:");
    Ui::result_ok(
        &format!("Jiji v{}", env!("CARGO_PKG_VERSION")),
        env!("JIJI_GIT_SHA"),
    );
}
