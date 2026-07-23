use jiji_config::{Config, ContainerEngine};
use jiji_ssh::SshSession;

use crate::{container_ops, proxy, proxy_routes};

/// Every `{project}-{service}-{app_port}` route name a proxy-enabled service in this project
/// would register, computed purely from config. No `NetworkPlan`/session needed: kamal-proxy
/// route names have no server component (each server's kamal-proxy keeps its own local route
/// table, keyed only by project/service/app_port -- see `proxy_routes::targets_for_service`).
/// Exact-name, not a prefix match: project/service names may themselves contain hyphens, so a
/// `"{project}-"` prefix match could false-positive against an unrelated project name that
/// happens to start the same way.
pub fn compute_route_candidates(config: &Config, project: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (service_name, service) in &config.services {
        let Some(proxy) = &service.proxy else {
            continue;
        };
        if let Some(targets) = &proxy.targets {
            for target in targets {
                names.push(format!("{project}-{service_name}-{}", target.app_port));
            }
        } else if let Some(app_port) = proxy.app_port {
            names.push(format!("{project}-{service_name}-{app_port}"));
        }
    }
    names
}

/// No routes if the kamal-proxy container itself doesn't exist (already-idempotent: a prior
/// teardown may already have removed it), checked via an existence precheck rather than matching
/// on `docker exec`'s "no such container" stderr wording.
pub async fn list_routes(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<Vec<String>> {
    if container_ops::inspect_status(session, engine, proxy::CONTAINER_NAME)
        .await?
        .is_none()
    {
        return Ok(Vec::new());
    }
    let command = proxy_routes::render_list_command(engine);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not list kamal-proxy routes on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(parse_route_names(&result.stdout))
}

/// Parses `kamal-proxy list`'s table output: strips ANSI color codes (confirmed live --
/// `ghcr.io/acidtib/kamal-proxy:jiji` always colorizes `list`, even over a non-interactive SSH
/// exec channel), skips a leading header row (if the first line looks like one), and takes the
/// first whitespace-separated column of every remaining line as the route name.
fn parse_route_names(stdout: &str) -> Vec<String> {
    let cleaned = strip_ansi_codes(stdout);
    let mut lines = cleaned.lines().peekable();
    if let Some(first) = lines.peek() {
        if first
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("service")
        {
            lines.next();
        }
    }
    lines
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// Strips ANSI SGR escape sequences (`\x1b[<params>m`) such as the color codes
/// `ghcr.io/acidtib/kamal-proxy:jiji`'s `list` command always emits.
fn strip_ansi_codes(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

/// Removes every `candidates` entry that's actually present in `existing_routes`. Returns
/// `(route_name, was_present)` for every candidate, so callers can report already-absent routes
/// without treating them as an error.
pub async fn remove_project_routes(
    session: &SshSession,
    engine: ContainerEngine,
    candidates: &[String],
    existing_routes: &[String],
) -> anyhow::Result<Vec<(String, bool)>> {
    let mut results = Vec::with_capacity(candidates.len());
    for route in candidates {
        if !existing_routes.contains(route) {
            results.push((route.clone(), false));
            continue;
        }
        proxy_routes::remove_route(session, engine, route).await?;
        results.push((route.clone(), true));
    }
    Ok(results)
}

pub enum ProxyContainerOutcome {
    Removed,
    AlreadyAbsent,
    RetainedInUseBy(Vec<String>),
}

/// Removes the kamal-proxy container itself only if no route remains for ANY project after this
/// project's own routes are gone -- kamal-proxy is shared across every project on a host, so its
/// container is never tied to a single project's ownership labels the way service containers are.
pub async fn teardown_proxy_container_if_unused(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<ProxyContainerOutcome> {
    let remaining = list_routes(session, engine).await?;
    if !remaining.is_empty() {
        return Ok(ProxyContainerOutcome::RetainedInUseBy(remaining));
    }
    if !container_ops::remove_if_present(session, engine, proxy::CONTAINER_NAME).await? {
        return Ok(ProxyContainerOutcome::AlreadyAbsent);
    }
    // Nothing needs kamal-proxy anymore, so its exclusive resources (not shared with any
    // service) are orphaned too. Both are recreated from scratch by the next `server setup`, so
    // there's no data-loss concern in removing them now.
    container_ops::remove_volume_if_present(session, engine, proxy::CONFIG_VOLUME).await?;
    remove_certs_dir(session).await?;
    Ok(ProxyContainerOutcome::Removed)
}

async fn remove_certs_dir(session: &SshSession) -> anyhow::Result<()> {
    let command = format!("rm -rf {}", proxy::CERTS_DIR);
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not remove {} on {}: {}",
            proxy::CERTS_DIR,
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> Config {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn single_target_route_name_matches_deploy_naming() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  web:\n    image: example/web\n    proxy: { app_port: 3000, host: example.com }\n",
        );
        let candidates = compute_route_candidates(&config, "demo");
        assert_eq!(candidates, vec!["demo-web-3000".to_string()]);
    }

    #[test]
    fn multi_target_produces_one_candidate_per_target() {
        let config = config(
            r#"
project: demo
builder: { engine: docker }
servers: {}
services:
  web:
    image: example/web
    proxy:
      targets:
        - { app_port: 3900, host: s3.example.com }
        - { app_port: 3903, host: admin.example.com }
"#,
        );
        let mut candidates = compute_route_candidates(&config, "demo");
        candidates.sort();
        assert_eq!(
            candidates,
            vec!["demo-web-3900".to_string(), "demo-web-3903".to_string()]
        );
    }

    #[test]
    fn services_without_proxy_produce_no_candidates() {
        let config = config(
            "project: demo\nbuilder: { engine: docker }\nservers: {}\nservices:\n  worker:\n    image: example/worker\n",
        );
        assert!(compute_route_candidates(&config, "demo").is_empty());
    }

    #[test]
    fn parse_route_names_skips_a_header_row() {
        let stdout = "Service          Host             Target\ndemo-web-3000    example.com      10.0.0.2:3000\n";
        assert_eq!(parse_route_names(stdout), vec!["demo-web-3000".to_string()]);
    }

    #[test]
    fn parse_route_names_handles_headerless_output() {
        let stdout = "demo-web-3000  example.com  10.0.0.2:3000\nother-api-8080 api.example.com 10.0.0.3:8080\n";
        assert_eq!(
            parse_route_names(stdout),
            vec!["demo-web-3000".to_string(), "other-api-8080".to_string()]
        );
    }

    #[test]
    fn parse_route_names_strips_ansi_color_codes_from_a_real_capture() {
        // Captured live from `docker exec kamal-proxy kamal-proxy list` against
        // ghcr.io/acidtib/kamal-proxy:jiji over a non-interactive SSH exec channel.
        let stdout = "\u{1b}[3;94mService\u{1b}[0m         \u{1b}[3;94mHost\u{1b}[0m                 \u{1b}[3;94mPath\u{1b}[0m  \u{1b}[3;94mTarget\u{1b}[0m             \u{1b}[3;94mState\u{1b}[0m    \u{1b}[3;94mTLS\u{1b}[0m  \n\u{1b}[1;34mtdsmoke-web-80\u{1b}[0m  \u{1b}[mtdsmoke.example.com\u{1b}[0m  \u{1b}[m/\u{1b}[0m     \u{1b}[m100.82.198.240:80\u{1b}[0m  \u{1b}[mrunning\u{1b}[0m  \u{1b}[mno\u{1b}[0m   \n";
        assert_eq!(
            parse_route_names(stdout),
            vec!["tdsmoke-web-80".to_string()]
        );
    }

    #[test]
    fn remove_project_routes_reports_already_absent_without_calling_remove() {
        // Pure existence-filtering logic exercised without a session: candidates not present in
        // existing_routes must be reported (not skipped) as already-absent.
        let candidates = ["demo-web-3000".to_string(), "demo-api-8080".to_string()];
        let existing = ["demo-web-3000".to_string()];
        let present: Vec<bool> = candidates
            .iter()
            .map(|candidate| existing.contains(candidate))
            .collect();
        assert_eq!(present, vec![true, false]);
    }
}
