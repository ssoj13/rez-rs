// SPDX-License-Identifier: Apache-2.0

//! `rez memcache` - manage and query memcache servers.

use clap::Args;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use foundation::errors::{Result, RezError};
use model::config::CONFIG;
use repository::{FsRepoMemcached, MemcacheClient, MemcacheProbeOutcome, PackageRepository};

/// Manage and query memcache server(s).
#[derive(Args, Debug)]
pub struct MemcacheArgs {
    /// Flush all cache entries
    #[arg(long)]
    pub flush: bool,

    /// Show cache statistics
    #[arg(long)]
    pub stats: bool,

    /// Reset server statistics
    #[arg(long)]
    pub reset_stats: bool,

    /// Continuously poll showing gets/sets per second
    #[arg(long)]
    pub poll: bool,

    /// Poll interval in seconds
    #[arg(long, default_value = "1.0")]
    pub interval: f64,

    /// Warm cache with visible packages
    #[arg(long)]
    pub warm: bool,

    /// Verbose output
    #[arg(from_global)]
    pub verbose: u8,
}

pub fn run(args: &MemcacheArgs) -> Result<()> {
    let uris = &CONFIG.memcached_uri;
    if uris.is_empty() {
        return Err(RezError::Config(
            "memcaching is not enabled. Set 'memcached_uri' in rezconfig to enable".into(),
        ));
    }

    let client = MemcacheClient::new(uris);

    // Keep Rez's command precedence: poll, flush, warm, reset, stats, summary.
    if args.poll {
        return poll(&client, args.interval);
    }

    if args.flush {
        client.flush()?;
        println!("memcached servers are flushed.");
        return Ok(());
    }

    if args.warm {
        warm(uris, args.verbose > 0)?;
        println!("memcached servers are warmed.");
        return Ok(());
    }

    if args.reset_stats {
        client.reset_stats()?;
        println!("memcached servers are stat reset.");
        return Ok(());
    }

    let stats = client.stats()?;
    if stats.is_empty() {
        return Err(RezError::System(
            "memcached servers are not responding.".into(),
        ));
    }

    if args.stats {
        let yaml = serde_yaml::to_string(&stats).map_err(|error| {
            RezError::ResourceContent(format!("Cannot serialize memcache stats: {error}"))
        })?;
        print!("{yaml}");
        return Ok(());
    }

    print!("{}", summary_table(&stats)?);
    Ok(())
}

fn warm(uris: &[String], verbose: bool) -> Result<()> {
    let paths = CONFIG.expanded_nonlocal_packages_path_os();
    let mut seen = HashSet::new();

    for path in paths {
        let Some(repository) = FsRepoMemcached::new(
            &path,
            MemcacheClient::new(uris),
            CONFIG.cache_listdir,
            CONFIG.cache_package_files,
            false,
        )?
        else {
            continue;
        };

        for family in repository.iter_family_names()? {
            for version in repository.iter_versions(&family)? {
                let key = (family.clone(), version.to_string());
                if seen.contains(&key) {
                    continue;
                }

                let Some(package) = repository.get_package(&family, &version)? else {
                    continue;
                };
                seen.insert(key);

                if verbose {
                    println!("warming: {}-{}", package.name, package.version);
                }
            }
        }
    }

    Ok(())
}

#[derive(Debug)]
struct PollSample {
    sampled_at: Instant,
    stats: Vec<(String, HashMap<String, String>)>,
}

#[derive(Debug, PartialEq)]
struct PollRates {
    gets_per_second: f64,
    sets_per_second: f64,
}

fn poll_candidate_servers(
    previous: &[(String, HashMap<String, String>)],
    current: &[(String, HashMap<String, String>)],
) -> Vec<String> {
    current
        .iter()
        .filter_map(|(server, current_stats)| {
            let previous_stats = previous
                .iter()
                .find(|(previous_server, _)| previous_server == server)
                .map(|(_, stats)| stats);

            if !current_stats.is_empty() && previous_stats.is_some_and(|stats| !stats.is_empty()) {
                Some(server.clone())
            } else {
                None
            }
        })
        .collect()
}

fn poll_rates(
    previous: &HashMap<String, String>,
    current: &HashMap<String, String>,
    elapsed: Duration,
) -> Result<PollRates> {
    if elapsed.is_zero() {
        return Err(RezError::Config(
            "memcache poll interval must be greater than zero".into(),
        ));
    }

    let parse_counter = |stats: &HashMap<String, String>, key: &str| -> Result<i128> {
        let value = stats.get(key).ok_or_else(|| {
            RezError::System(format!("memcache stats response is missing '{key}'"))
        })?;
        value.parse::<i128>().map_err(|error| {
            RezError::System(format!(
                "invalid memcache counter '{key}' value {value:?}: {error}"
            ))
        })
    };

    let elapsed_secs = elapsed.as_secs_f64();
    let gets = parse_counter(current, "cmd_get")? - parse_counter(previous, "cmd_get")?;
    let sets = parse_counter(current, "cmd_set")? - parse_counter(previous, "cmd_set")?;

    Ok(PollRates {
        gets_per_second: gets as f64 / elapsed_secs,
        sets_per_second: sets as f64 / elapsed_secs,
    })
}

fn poll(client: &MemcacheClient, interval_secs: f64) -> Result<()> {
    if !interval_secs.is_finite() || interval_secs <= 0.0 {
        return Err(RezError::Config(
            "memcache poll interval must be a finite value greater than zero".into(),
        ));
    }
    let interval = Duration::try_from_secs_f64(interval_secs)
        .map_err(|error| RezError::Config(format!("invalid memcache poll interval: {error}")))?;

    println!(
        "{:<64} {:<16} {:<16} {:<16} {:<16} {:<16}",
        "SERVER", "CONNS", "GET/s", "SET/s", "TEST_GET", "TEST_SET"
    );

    let mut previous: Option<PollSample> = None;
    loop {
        let stats = client.stats()?;
        let now = Instant::now();

        if let Some(previous_sample) = previous.as_ref() {
            let elapsed = now.duration_since(previous_sample.sampled_at);
            if !elapsed.is_zero() {
                let available_servers = poll_candidate_servers(&previous_sample.stats, &stats);
                let probes = client.probe_servers(Some(&available_servers))?;
                let probe_by_server: HashMap<String, _> = probes
                    .into_iter()
                    .filter_map(|outcome| match outcome {
                        MemcacheProbeOutcome::Available(probe) => {
                            Some((probe.server.clone(), probe))
                        }
                        MemcacheProbeOutcome::Unavailable { .. } => None,
                    })
                    .collect();

                for (server, current) in &stats {
                    let Some((_, old)) = previous_sample
                        .stats
                        .iter()
                        .find(|(previous_server, _)| previous_server == server)
                    else {
                        continue;
                    };
                    if current.is_empty() || old.is_empty() {
                        continue;
                    }

                    let rates = poll_rates(old, current, elapsed)?;
                    let Some(probe) = probe_by_server.get(server) else {
                        continue;
                    };
                    let connections = current
                        .get("curr_connections")
                        .ok_or_else(|| {
                            RezError::System(format!(
                                "memcache stats response from {server:?} is missing 'curr_connections'"
                            ))
                        })?
                        .parse::<u64>()
                        .map_err(|error| {
                            RezError::System(format!(
                                "invalid memcache curr_connections from {server:?}: {error}"
                            ))
                        })?;

                    println!(
                        "{:<64} {:<16} {:<16} {:<16} {:<16} {:<16}",
                        server,
                        connections,
                        rates.gets_per_second,
                        rates.sets_per_second,
                        probe.get_time.as_secs_f64(),
                        probe.set_time.as_secs_f64()
                    );
                }
            }
        }

        previous = Some(PollSample {
            sampled_at: now,
            stats,
        });
        std::thread::sleep(interval);
    }
}

fn summary_table(stats: &[(String, HashMap<String, String>)]) -> Result<String> {
    let mut rows = vec![
        vec![
            "CACHE SERVER".to_owned(),
            "UPTIME".to_owned(),
            "HITS".to_owned(),
            "MISSES".to_owned(),
            "HIT RATIO".to_owned(),
            "MEMORY".to_owned(),
            "USED".to_owned(),
        ],
        vec![
            "------------".to_owned(),
            "------".to_owned(),
            "----".to_owned(),
            "------".to_owned(),
            "---------".to_owned(),
            "------".to_owned(),
            "----".to_owned(),
        ],
    ];

    for (server, data) in stats {
        let uptime = stat_number(data, "uptime")?;
        let hits = stat_number(data, "get_hits")?;
        let misses = stat_number(data, "get_misses")?;
        let memory = stat_number(data, "limit_maxbytes")?;
        let used = stat_number(data, "bytes")?;

        let hit_ratio = (hits as f64 / (hits + misses).max(1) as f64 * 100.0) as u128;
        let used_ratio = (used as f64 / memory.max(1) as f64 * 100.0) as u128;
        rows.push(vec![
            server
                .split_whitespace()
                .next()
                .unwrap_or(server)
                .to_owned(),
            readable_time_duration(uptime),
            hits.to_string(),
            misses.to_string(),
            format!("{hit_ratio}%"),
            readable_memory_size(memory),
            format!("{} ({used_ratio}%)", readable_memory_size(used)),
        ]);
    }

    let widths: Vec<usize> = (0..rows[0].len())
        .map(|column| rows.iter().map(|row| row[column].len()).max().unwrap_or(0))
        .collect();
    let mut output = String::new();
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            if index > 0 {
                output.push(' ');
            }
            output.push_str(&format!("{value:<width$}", width = widths[index]));
        }
        output.push('\n');
    }
    Ok(output)
}

fn stat_number(stats: &HashMap<String, String>, key: &str) -> Result<u128> {
    let value = stats.get(key).map(String::as_str).unwrap_or("0");
    value.parse::<u128>().map_err(|error| {
        RezError::System(format!(
            "invalid memcache stat '{key}' value {value:?}: {error}"
        ))
    })
}

fn readable_time_duration(seconds: u128) -> String {
    format_units(
        seconds,
        &[
            (365 * 24 * 3600, "years", 10),
            (30 * 24 * 3600, "months", 12),
            (7 * 24 * 3600, "weeks", 5),
            (24 * 3600, "days", 7),
            (3600, "hours", 10),
            (60, "minutes", 10),
            (1, "seconds", 60),
        ],
        true,
    )
}

fn readable_memory_size(bytes: u128) -> String {
    format_units(
        bytes,
        &[
            (1024_u128.pow(4), "Tb", 128),
            (1024_u128.pow(3), "Gb", 64),
            (1024_u128.pow(2), "Mb", 32),
            (1024, "Kb", 16),
            (1, "bytes", 1024),
        ],
        false,
    )
}

fn format_units(value: u128, units: &[(u128, &str, u128)], plural_aware: bool) -> String {
    if value == 0 {
        return format!("0 {}", units.last().map(|unit| unit.1).unwrap_or("units"));
    }

    for (quantity, unit, threshold) in units {
        if value < *quantity {
            continue;
        }

        let scaled = value as f64 / *quantity as f64;
        let rounded = if scaled > *threshold as f64 {
            scaled.round()
        } else {
            (scaled * 10.0).round() / 10.0
        };
        let singular = plural_aware && (rounded - 1.0).abs() < 1e-9;
        let unit = *unit;
        let unit = if singular {
            unit.strip_suffix('s').unwrap_or(unit)
        } else {
            unit
        };
        return format!("{rounded} {unit}");
    }

    format!("0 {}", units.last().map(|unit| unit.1).unwrap_or("units"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn poll_probes_only_nonempty_servers_present_in_both_samples() {
        let previous = vec![
            ("existing".to_owned(), stats(&[("cmd_get", "1")])),
            ("empty".to_owned(), HashMap::new()),
            ("gone".to_owned(), stats(&[("cmd_get", "2")])),
        ];
        let current = vec![
            ("existing".to_owned(), stats(&[("cmd_get", "3")])),
            ("empty".to_owned(), stats(&[("cmd_get", "4")])),
            ("new".to_owned(), stats(&[("cmd_get", "1")])),
        ];

        assert_eq!(
            poll_candidate_servers(&previous, &current),
            vec!["existing".to_owned()]
        );
    }

    #[test]
    fn poll_rates_use_counter_deltas_and_elapsed_interval() {
        let previous = stats(&[("cmd_get", "10"), ("cmd_set", "4")]);
        let current = stats(&[("cmd_get", "30"), ("cmd_set", "10")]);

        let rates = poll_rates(&previous, &current, Duration::from_secs(2)).unwrap();

        assert_eq!(
            rates,
            PollRates {
                gets_per_second: 10.0,
                sets_per_second: 3.0,
            }
        );
    }

    #[test]
    fn poll_rates_preserve_negative_deltas_after_server_stat_reset() {
        let previous = stats(&[("cmd_get", "5"), ("cmd_set", "2")]);
        let current = stats(&[("cmd_get", "1"), ("cmd_set", "0")]);

        let rates = poll_rates(&previous, &current, Duration::from_secs(2)).unwrap();

        assert_eq!(
            rates,
            PollRates {
                gets_per_second: -2.0,
                sets_per_second: -1.0,
            }
        );
    }

    #[test]
    fn summary_uses_capacity_for_memory_and_formats_usage() {
        let values = stats(&[
            ("uptime", "1"),
            ("get_hits", "9"),
            ("get_misses", "1"),
            ("limit_maxbytes", "1024"),
            ("bytes", "512"),
        ]);
        let rows = vec![("memcache://127.0.0.1:11211".to_owned(), values)];

        let output = summary_table(&rows).unwrap();

        assert!(output.contains("90%"));
        assert!(output.contains("1 Kb"));
        assert!(output.contains("512 bytes (50%)"));
    }

    #[test]
    fn summary_handles_empty_and_missing_counters_as_zero() {
        let output = summary_table(&[("cache".to_owned(), HashMap::new())]).unwrap();

        assert!(output.contains("0 seconds"));
        assert!(output.contains("0 bytes"));
        assert!(output.contains("0%"));
    }

    #[test]
    fn summary_propagates_malformed_numeric_stats() {
        let values = stats(&[("uptime", "not-a-number")]);

        assert!(summary_table(&[("cache".to_owned(), values)]).is_err());
    }
}
