pub mod list;
pub mod logs;
pub mod run;
pub mod status;

use jiji_config::{Config, CronConfig};

use crate::commands::deploy::split_comma_trimmed;

/// Resolves the `-S`/`--services` filter down to services that define at least one cron job.
/// An empty filter matches every such service. Sorted by name for stable output.
pub(crate) fn select_cron_services<'a>(
    config: &'a Config,
    services_filter: Option<&str>,
) -> Vec<(&'a str, &'a CronConfig, &'a str)> {
    let filters = split_comma_trimmed(services_filter);
    let mut rows: Vec<(&str, &CronConfig, &str)> = config
        .services
        .iter()
        .filter(|(name, _)| {
            filters.is_empty()
                || filters
                    .iter()
                    .any(|filter| jiji_core::matches_pattern(name, filter))
        })
        .flat_map(|(service_name, service)| {
            service
                .crons
                .iter()
                .map(move |(cron_name, cron)| (service_name.as_str(), cron, cron_name.as_str()))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(b.0).then(a.2.cmp(b.2)));
    rows
}

/// Resolves `-S`/`--services` to exactly one service and one cron name within it, as required by
/// `jiji service cron logs`/`run`. Returns an actionable error for zero matches, more than one
/// matched service, or an unknown cron name.
pub(crate) fn select_single_cron<'a>(
    config: &'a Config,
    services_filter: Option<&str>,
    cron_name: &str,
) -> anyhow::Result<(&'a str, &'a CronConfig)> {
    let filters = split_comma_trimmed(services_filter);
    let mut matched: Vec<&str> = config
        .services
        .iter()
        .filter(|(name, service)| {
            !service.crons.is_empty()
                && (filters.is_empty()
                    || filters
                        .iter()
                        .any(|filter| jiji_core::matches_pattern(name, filter)))
        })
        .map(|(name, _)| name.as_str())
        .collect();
    matched.sort_unstable();

    match matched.as_slice() {
        [] => anyhow::bail!(
            "No service with cron jobs matched -S '{}'. Set -S to a service with a `crons:` map.",
            filters.join(",")
        ),
        [service_name] => {
            let service = &config.services[*service_name];
            let cron = service.crons.get(cron_name).ok_or_else(|| {
                let mut available: Vec<&str> =
                    service.crons.keys().map(String::as_str).collect();
                available.sort_unstable();
                anyhow::anyhow!(
                    "Service '{service_name}' has no cron named '{cron_name}'. Available: {}",
                    available.join(", ")
                )
            })?;
            Ok((service_name, cron))
        }
        many => anyhow::bail!(
            "-S matched {} services with cron jobs ({}); this command requires exactly one. Narrow -S to a single service.",
            many.len(),
            many.join(", ")
        ),
    }
}
