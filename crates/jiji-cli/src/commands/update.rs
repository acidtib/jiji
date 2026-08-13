//! `jiji update`: thin orchestration over `self_update`'s pure/network helpers, config-free like
//! `commands::init`/`commands::version`.

use std::path::PathBuf;

use jiji_tui::Ui;

use crate::self_update;

/// Reads `JIJI_UPDATE_TARGET_PATH` (same override shape as `agent_distribution`'s env knobs) so
/// tests never overwrite the real test binary; defaults to the running executable's own path.
fn target_path() -> anyhow::Result<PathBuf> {
    if let Ok(path) = std::env::var("JIJI_UPDATE_TARGET_PATH") {
        if !path.trim().is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    std::env::current_exe().map_err(|error| {
        anyhow::anyhow!("could not determine the running jiji binary's path: {error}")
    })
}

pub async fn run(check: bool, version: Option<&str>) -> anyhow::Result<()> {
    Ui::section("Update:");

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let asset = self_update::artifact_name(os, arch)?;

    // Without a timeout, a hung GitHub connection (or a mock/mirror behind
    // `JIJI_RELEASE_API_BASE_URL`/`JIJI_RELEASE_BASE_URL` that never responds) blocks `jiji
    // update` indefinitely across up to three sequential requests (release list, sha256 sidecar,
    // asset). 30s comfortably covers a large asset download over a slow connection while still
    // failing actionably instead of hanging forever.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| anyhow::anyhow!("could not build an HTTP client: {error}"))?;
    let api_base_url = self_update::release_api_base_url();
    let base_url = self_update::release_base_url();

    let installed = format!("v{}", env!("CARGO_PKG_VERSION"));
    let target = self_update::resolve_target_version(&client, &api_base_url, version).await?;
    let up_to_date = self_update::is_up_to_date(&installed, &target);

    // An explicit --release always proceeds, even to an older release (the rollback
    // requirement): only an unpinned "give me latest" run can be short-circuited by
    // `up_to_date`.
    if version.is_none() && up_to_date {
        Ui::result_ok("Installed:", &installed);
        Ui::result_ok("Latest:", &target);
        Ui::say("jiji is already up to date.", 1);
        return Ok(());
    }

    if check {
        Ui::result_ok("Installed:", &installed);
        Ui::result_ok("Available:", &target);
        match version {
            Some(_) => Ui::say(
                &format!("Run `jiji update --release {target}` to install it."),
                1,
            ),
            None => Ui::say(&format!("Run `jiji update` to install {target}."), 1),
        }
        return Ok(());
    }

    let target_path = target_path()?;
    Ui::say(&format!("Downloading {target} ({asset})..."), 1);
    let bytes = self_update::fetch_and_verify_asset(&client, &base_url, &target, asset).await?;
    self_update::install_atomically(&target_path, &bytes)?;

    Ui::result_ok("Installed:", &target);
    Ui::say(
        "Run `jiji server upgrade -e <environment>` for each environment configuration to update your servers.",
        1,
    );
    Ok(())
}
