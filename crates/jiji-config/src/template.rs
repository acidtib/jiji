/// The default config template written by `jiji init`, embedded at compile time.
pub const TEMPLATE: &str = include_str!("jiji.yml");

/// Scans the template text for the first `engine:` line and returns its value
/// with a naive text scan on the template (not the parsed config).
pub fn template_engine() -> Option<&'static str> {
    TEMPLATE
        .lines()
        .find(|line| line.starts_with("engine:"))
        .and_then(|line| line.split(':').nth(1))
        .map(|v| v.trim())
}
