// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Container images, containers, build caches and volumes. `STO-13`.
//!
//! # What is here on a developer's machine
//!
//! Measured on the machine this was developed on:
//!
//! | | total | reclaimable |
//! | --- | --- | --- |
//! | Images | 28.92 GB across 75 | **17.5 GB** |
//! | Build cache | 3.04 GB across 180 | **3.04 GB** |
//! | Local volumes | 2.40 GB across 25 | **1.49 GB** |
//! | Containers | 38.27 MB across 28 | 94 kB |
//!
//! # Docker reports in powers of ten
//!
//! `docker system df` formats sizes with Go's `units.HumanSize`, which is **decimal**: its `GB` is
//! 10⁹, not 2³⁰. Verified rather than assumed — `docker images` reports one image as `314MB` and
//! `docker image inspect` gives `314319387` bytes for the same image, which is decimal to three
//! figures and would be 300 MB read as binary.
//!
//! This is the second time a tool's own units have been the trap; APT does the same thing, and reading
//! either as binary overstates by 5% for MB and 7% for GB.
//!
//! # Volumes are different from everything else
//!
//! Images, containers and build caches are reproducible: an image can be pulled again and a cache
//! rebuilt. A **volume holds the only copy of something** — a database's data directory lives in one.
//!
//! So volumes are `Risky`, are never bulk-selectable, and get one candidate each rather than a single
//! "prune volumes" button. The specification requires per-item confirmation and that is what
//! one-candidate-per-volume means in practice.
//!
//! # Privilege
//!
//! Docker is reachable without privilege when the user is in the `docker` group, which is the usual
//! developer setup and the case here. When it is not, talking to the daemon needs root — and rather
//! than ship a privileged Docker path that has never been exercised, this category reports itself
//! unavailable and says why. That is the same line drawn for `ostree` in `STO-12`.

use crate::error::{AppError, ErrorCode, Result};
use crate::op::CancelToken;
use crate::space::{Category as SpaceCategory, PruneScope, ReclaimMethod, Reclaimable, Safety};

use super::registry::{Candidate, Category};

/// One row of `docker system df`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Usage {
    /// `Images`, `Containers`, `Local Volumes`, `Build Cache`.
    pub(crate) kind: String,
    pub(crate) total: u64,
    pub(crate) active: u64,
    pub(crate) size: u64,
    pub(crate) reclaimable: u64,
}

/// Parse a decimal size as Docker formats it: `314MB`, `17.5GB`, `94.31kB`, `0B`.
///
/// Powers of ten, because that is what Go's `units.HumanSize` produces. Reading `GB` as 2³⁰ would
/// overstate by 7%, which is more than three times the specification's tolerance.
#[must_use]
pub(crate) fn parse_docker_size(raw: &str) -> u64 {
    // `system df` renders the reclaimable column as `17.5GB (60%)`; the percentage is noise here.
    let text = raw.split(" (").next().unwrap_or(raw).trim();

    let digits_end = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(digits_end);
    let Ok(value) = number.parse::<f64>() else {
        return 0;
    };

    let multiplier: f64 = match unit.trim() {
        "B" | "" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "PB" => 1e15,
        // An unrecognised unit must not be guessed at as bytes: that would silently understate by
        // orders of magnitude. Nothing is claimed instead.
        _ => return 0,
    };

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bytes = (value * multiplier) as u64;
    bytes
}

/// Parse `docker system df --format '{{json .}}'`, which emits one JSON object per line.
#[must_use]
pub(crate) fn parse_system_df(output: &str) -> Vec<Usage> {
    output
        .lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
            let field = |k: &str| -> String {
                value
                    .get(k)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let kind = field("Type");
            if kind.is_empty() {
                return None;
            }
            Some(Usage {
                total: field("TotalCount").parse().unwrap_or(0),
                active: field("Active").parse().unwrap_or(0),
                size: parse_docker_size(&field("Size")),
                reclaimable: parse_docker_size(&field("Reclaimable")),
                kind,
            })
        })
        .collect()
}

/// One local volume, as `docker volume ls` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Volume {
    pub(crate) name: String,
    /// Bytes it occupies, when `docker system df -v` could be read.
    pub(crate) bytes: u64,
    /// How many containers refer to it. Zero means nothing is using it.
    pub(crate) links: u64,
}

/// Whether a string is shaped like a Docker volume name.
///
/// Docker's own rule is `[a-zA-Z0-9][a-zA-Z0-9_.-]*`. Checked because the name reaches a command line;
/// it is a second guard behind the primary one, which is that the name must appear in the set the
/// daemon itself reported as unused.
#[must_use]
pub(crate) fn is_volume_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    name.len() <= 255
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// Parse the `Local Volumes space usage` block of `docker system df -v`.
///
/// ```text
/// Local Volumes space usage:
///
/// VOLUME NAME                        LINKS     SIZE
/// 359964401c5cb...                   1         73.01MB
/// drugcheck_scraper_postgres_dev...  0         198MB
/// ```
#[must_use]
pub(crate) fn parse_volumes(output: &str) -> Vec<Volume> {
    let mut volumes = Vec::new();
    let mut in_block = false;

    for line in output.lines() {
        if line.starts_with("Local Volumes space usage") {
            in_block = true;
            continue;
        }
        if in_block {
            // Another section's header ends the block.
            if line.ends_with("space usage:") {
                break;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 3 || fields[0] == "VOLUME" {
                continue;
            }
            // Name, links, size — read from the end, because a name cannot contain whitespace but
            // this is more robust to column padding than fixed offsets.
            let size = parse_docker_size(fields[fields.len() - 1]);
            let Ok(links) = fields[fields.len() - 2].parse::<u64>() else {
                continue;
            };
            let name = fields[0].to_string();
            if !is_volume_name(&name) {
                continue;
            }
            volumes.push(Volume {
                name,
                bytes: size,
                links,
            });
        }
    }
    volumes
}

/// Run a docker subcommand, capturing failure as a typed error.
fn docker(args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("docker")
        .args(args)
        .output()
        .map_err(|e| AppError::from_io(&e, "run docker"))?;

    if !output.status.success() {
        return Err(AppError::new(
            ErrorCode::CommandFailed,
            "Docker did not answer.",
        )
        .with_remedy(
            "nix talks to Docker as your own user. If you are not in the `docker` group, Docker needs \
             root and nix will not run privileged Docker commands it cannot test.",
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Container storage, via the Docker CLI.
#[derive(Debug, Default)]
pub struct ContainerCategory;

impl ContainerCategory {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Whether the daemon actually answers as this user.
    fn reachable() -> bool {
        crate::caps::registry().has(crate::caps::Capability::Docker)
            && docker(&["system", "df", "--format", "{{json .}}"]).is_ok()
    }
}

impl Category for ContainerCategory {
    fn id(&self) -> &'static str {
        "containers"
    }

    fn label(&self) -> &'static str {
        "Container storage"
    }

    fn explains(&self) -> &'static str {
        "Images, stopped containers and build layers Docker is holding. An image still referenced is downloaded again the next time you run it, which needs a network connection and can be several gigabytes. Running containers and their volumes are never touched."
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::ContainerImage
    }

    fn available(&self) -> bool {
        Self::reachable()
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        token.check()?;
        if !crate::caps::registry().has(crate::caps::Capability::Docker) {
            return Ok(Vec::new());
        }
        let Ok(df) = docker(&["system", "df", "--format", "{{json .}}"]) else {
            return Ok(Vec::new());
        };
        let usage = parse_system_df(&df);
        token.check()?;

        let mut candidates = Vec::new();
        let find = |kind: &str| usage.iter().find(|u| u.kind == kind);

        if let Some(images) = find("Images") {
            if images.reclaimable > 0 {
                // Two separate offers, because they are very different decisions. Dangling layers
                // belong to no tagged image and nothing can be using them; unused images are tagged
                // things a user may well want again, and removing them means re-pulling.
                candidates.push(Candidate {
                    path: std::path::PathBuf::from("docker dangling images"),
                    label: "Dangling image layers".to_string(),
                    bytes: images.reclaimable,
                    safety: Safety::Review,
                    method: ReclaimMethod::ContainerPrune {
                        scope: PruneScope::DanglingImages,
                    },
                    cost: Some(
                        "Removes layers belonging to no tagged image. Rebuilding an image that shared \
                         them will take longer."
                            .to_string(),
                    ),
                    category: self.id().to_string(),
                    // Docker reports what *it* considers reclaimable, and layers are shared between
                    // images — so the figure is its estimate, not a proof about the filesystem.
                    reclaimable: Reclaimable::AtMost {
                        exclusive: None,
                        reason: "Docker shares layers between images, so how much a prune returns is \
                                 its accounting rather than a per-file measurement."
                            .to_string(),
                    },
                });

                let unused = images.size.saturating_sub(images.reclaimable);
                if images.total > images.active && unused > 0 {
                    candidates.push(Candidate {
                        path: std::path::PathBuf::from("docker unused images"),
                        label: format!(
                            "Images no container uses ({} of {})",
                            images.total - images.active,
                            images.total
                        ),
                        bytes: images.reclaimable,
                        safety: Safety::Risky,
                        method: ReclaimMethod::ContainerPrune {
                            scope: PruneScope::UnusedImages,
                        },
                        cost: Some(
                            "Removes every image not currently used by a container, tagged ones \
                             included. Anything you want again will have to be pulled or rebuilt."
                                .to_string(),
                        ),
                        category: self.id().to_string(),
                        reclaimable: Reclaimable::AtMost {
                            exclusive: None,
                            reason:
                                "Docker decides which images are unused and shares layers between \
                                     them, so the figure is its estimate."
                                    .to_string(),
                        },
                    });
                }
            }
        }

        if let Some(containers) = find("Containers") {
            if containers.reclaimable > 0 {
                candidates.push(Candidate {
                    path: std::path::PathBuf::from("docker stopped containers"),
                    label: format!("Stopped containers ({})", containers.total - containers.active),
                    bytes: containers.reclaimable,
                    safety: Safety::Review,
                    method: ReclaimMethod::ContainerPrune {
                        scope: PruneScope::StoppedContainers,
                    },
                    cost: Some(
                        "Removes exited containers and their writable layers. Anything written inside \
                         one that is not on a volume is lost."
                            .to_string(),
                    ),
                    category: self.id().to_string(),
                    reclaimable: Reclaimable::Exact,
                });
            }
        }

        if let Some(cache) = find("Build Cache") {
            if cache.reclaimable > 0 {
                candidates.push(Candidate {
                    path: std::path::PathBuf::from("docker build cache"),
                    label: format!("Build cache ({} entries)", cache.total),
                    bytes: cache.reclaimable,
                    safety: Safety::Review,
                    method: ReclaimMethod::ContainerPrune {
                        scope: PruneScope::BuildCache,
                    },
                    cost: Some(
                        "The next image build repeats every step instead of reusing cached layers."
                            .to_string(),
                    ),
                    category: self.id().to_string(),
                    reclaimable: Reclaimable::Exact,
                });
            }
        }

        // Volumes, one at a time. Never a single "prune volumes" button.
        if let Ok(verbose) = docker(&["system", "df", "-v"]) {
            for volume in parse_volumes(&verbose) {
                token.check()?;
                if volume.links > 0 || volume.bytes == 0 {
                    continue;
                }
                candidates.push(Candidate {
                    path: std::path::PathBuf::from(format!("docker volume {}", volume.name)),
                    label: format!("Volume {}", volume.name),
                    bytes: volume.bytes,
                    // The only `Risky` rating in the storage half of nix, and deliberately so: a
                    // volume is where a container's data actually lives, and nothing else has a copy.
                    safety: Safety::Risky,
                    method: ReclaimMethod::ContainerVolume {
                        name: volume.name.clone(),
                    },
                    cost: Some(format!(
                        "Deletes the contents of {} permanently. If this held a database, that data is \
                         gone and nothing else has it.",
                        volume.name
                    )),
                    category: self.id().to_string(),
                    reclaimable: Reclaimable::Exact,
                });
            }
        }

        candidates.sort_by_key(|c| std::cmp::Reverse(c.bytes));
        Ok(candidates)
    }
}

/// The fixed argument vector for each prune scope. Nothing here is caller-supplied.
#[must_use]
pub(crate) const fn prune_command(scope: PruneScope) -> &'static [&'static str] {
    match scope {
        PruneScope::DanglingImages => &["image", "prune", "-f"],
        PruneScope::UnusedImages => &["image", "prune", "-a", "-f"],
        PruneScope::StoppedContainers => &["container", "prune", "-f"],
        PruneScope::BuildCache => &["builder", "prune", "-f"],
    }
}

/// Run a prune and report what Docker says it reclaimed.
pub(super) fn prune(scope: PruneScope) -> Result<u64> {
    let output = docker(prune_command(scope))?;
    Ok(parse_reclaimed(&output))
}

/// Remove one volume, having first confirmed the daemon still considers it unused.
///
/// The check is the point. A volume that acquired a container between the preview and now must not be
/// removed, and the only authority on that is Docker.
pub(super) fn remove_volume(name: &str) -> Result<u64> {
    if !is_volume_name(name) {
        return Err(AppError::invalid_input(format!(
            "{name} is not shaped like a Docker volume name."
        )));
    }

    let verbose = docker(&["system", "df", "-v"])?;
    let Some(volume) = parse_volumes(&verbose)
        .into_iter()
        .find(|v| v.name == name && v.links == 0)
    else {
        return Err(AppError::new(
            ErrorCode::HelperRejected,
            format!("{name} is not an unused volume."),
        )
        .with_remedy(
            "Docker is asked again immediately before removal, and it no longer reports this volume \
             as unused — something may have started using it.",
        ));
    };

    docker(&["volume", "rm", name])?;
    Ok(volume.bytes)
}

/// Docker's pruning commands end with `Total reclaimed space: 3.042GB`.
#[must_use]
pub(crate) fn parse_reclaimed(output: &str) -> u64 {
    output
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("Total reclaimed space:"))
        .map(|value| parse_docker_size(value.trim()))
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Captured from the development machine.
    const REAL_DF: &str = r#"{"Active":"25","Reclaimable":"17.5GB (60%)","Size":"28.92GB","TotalCount":"75","Type":"Images"}
{"Active":"27","Reclaimable":"94.31kB (0%)","Size":"38.27MB","TotalCount":"28","Type":"Containers"}
{"Active":"7","Reclaimable":"1.494GB (62%)","Size":"2.398GB","TotalCount":"25","Type":"Local Volumes"}
{"Active":"0","Reclaimable":"3.042GB","Size":"3.042GB","TotalCount":"180","Type":"Build Cache"}"#;

    const REAL_VERBOSE: &str = "\
Local Volumes space usage:

VOLUME NAME                                                        LINKS     SIZE
359964401c5cb84a1c6ed4391e8c9c495200bc2988e141edb4a688617bad2a5e   1         73.01MB
drugcheck_scraper_postgres_dev_data                                0         198MB
empty_vol                                                          0         0B

Build cache usage: 3.042GB
";

    /// Verified against the daemon, not assumed: `docker images` says `314MB` for an image that
    /// `docker image inspect` reports as 314,319,387 bytes.
    #[test]
    fn docker_sizes_are_powers_of_ten() {
        assert_eq!(parse_docker_size("314MB"), 314_000_000);
        assert_eq!(parse_docker_size("17.5GB"), 17_500_000_000);
        assert_eq!(parse_docker_size("94.31kB"), 94_310);
        assert_eq!(parse_docker_size("0B"), 0);
        assert_eq!(parse_docker_size("2.398GB"), 2_398_000_000);

        // Read as binary, 17.5GB would be 18,790,481,920 — 7% high, more than three times the
        // specification's tolerance.
        assert_ne!(parse_docker_size("17.5GB"), 17_500 * 1024 * 1024);
    }

    #[test]
    fn the_reclaimable_percentage_is_not_mistaken_for_a_size() {
        assert_eq!(parse_docker_size("17.5GB (60%)"), 17_500_000_000);
        assert_eq!(parse_docker_size("94.31kB (0%)"), 94_310);
    }

    /// An unrecognised unit must claim nothing rather than fall back to bytes.
    #[test]
    fn an_unknown_unit_claims_nothing() {
        assert_eq!(parse_docker_size("12ZB"), 0);
        assert_eq!(parse_docker_size("nonsense"), 0);
        assert_eq!(parse_docker_size(""), 0);
    }

    #[test]
    fn system_df_is_parsed_from_real_output() {
        let usage = parse_system_df(REAL_DF);
        assert_eq!(usage.len(), 4);

        let images = usage.iter().find(|u| u.kind == "Images").unwrap();
        assert_eq!(images.total, 75);
        assert_eq!(images.active, 25);
        assert_eq!(images.size, 28_920_000_000);
        assert_eq!(images.reclaimable, 17_500_000_000);

        let cache = usage.iter().find(|u| u.kind == "Build Cache").unwrap();
        assert_eq!(cache.total, 180);
        assert_eq!(cache.reclaimable, 3_042_000_000);
    }

    #[test]
    fn malformed_lines_are_dropped_rather_than_guessed_at() {
        assert!(parse_system_df("not json\n{}\n").is_empty());
        assert!(parse_system_df("").is_empty());
    }

    #[test]
    fn volumes_are_parsed_with_their_link_counts() {
        let volumes = parse_volumes(REAL_VERBOSE);
        assert_eq!(volumes.len(), 3, "{volumes:?}");

        let used = &volumes[0];
        assert_eq!(used.links, 1);
        assert_eq!(used.bytes, 73_010_000);

        let unused = volumes
            .iter()
            .find(|v| v.name.starts_with("drugcheck"))
            .unwrap();
        assert_eq!(unused.links, 0);
        assert_eq!(unused.bytes, 198_000_000);
    }

    #[test]
    fn the_volume_block_ends_at_the_next_section() {
        let volumes = parse_volumes(REAL_VERBOSE);
        assert!(
            !volumes.iter().any(|v| v.name.starts_with("Build")),
            "the next section's header must not become a volume: {volumes:?}"
        );
    }

    /// Volume names reach a command line, so their shape is checked independently.
    #[test]
    fn volume_names_are_shape_checked() {
        for good in ["mydata", "a", "app_db-1.0", "359964401c5cb84a"] {
            assert!(is_volume_name(good), "{good} is a valid volume name");
        }
        for bad in [
            "",
            "-leading",
            ".leading",
            "has space",
            "semi;colon",
            "--rm",
            "a/b",
            "$(x)",
        ] {
            assert!(!is_volume_name(bad), "{bad} must be rejected");
        }
        assert!(
            !is_volume_name(&"a".repeat(300)),
            "absurd lengths are rejected"
        );
    }

    #[test]
    fn prune_commands_are_fixed_and_distinct() {
        let mut seen = std::collections::HashSet::new();
        for scope in [
            PruneScope::DanglingImages,
            PruneScope::UnusedImages,
            PruneScope::StoppedContainers,
            PruneScope::BuildCache,
        ] {
            let args = prune_command(scope);
            assert!(
                seen.insert(args),
                "{scope:?} shares a command with another scope"
            );
            assert!(
                args.contains(&"-f"),
                "{scope:?} must not wait for confirmation nix has already obtained"
            );
            assert!(!scope.name().is_empty());
        }
    }

    #[test]
    fn reclaimed_space_is_read_from_docker_own_report() {
        let output = "deleted: sha256:abc\ndeleted: sha256:def\n\nTotal reclaimed space: 3.042GB\n";
        assert_eq!(parse_reclaimed(output), 3_042_000_000);
        assert_eq!(parse_reclaimed("nothing to do\n"), 0);
    }

    #[test]
    fn cancellation_is_honoured() {
        let cancelled = CancelToken::new();
        cancelled.cancel();
        match ContainerCategory::new().candidates(&cancelled) {
            Err(e) => assert!(!e.is_fault(), "cancellation is not a fault"),
            Ok(found) => assert!(found.is_empty()),
        }
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_containers_are_read_consistently() {
        let category = ContainerCategory::new();
        if !category.available() {
            return;
        }
        let found = category.candidates(&CancelToken::new()).unwrap();

        for c in &found {
            assert!(c.bytes > 0, "{} claims nothing", c.label);
            assert!(
                c.cost.as_deref().is_some_and(|s| !s.is_empty()),
                "{} must say what it costs",
                c.label
            );
            assert_ne!(c.safety, Safety::Safe, "nothing here is pre-checkable");
        }

        // Every volume candidate is its own item, and none may be bulk-selected.
        for c in found
            .iter()
            .filter(|c| matches!(c.method, ReclaimMethod::ContainerVolume { .. }))
        {
            assert_eq!(c.safety, Safety::Risky, "{} must be Risky", c.label);
            assert!(
                !c.safety.bulk_selectable(),
                "a volume must never be swept up by select-all"
            );
        }
    }

    /// The specification's criterion: the preview total agrees with `docker system df`.
    #[test]
    fn the_preview_agrees_with_docker_system_df() {
        let category = ContainerCategory::new();
        if !category.available() {
            return;
        }
        let Ok(df) = docker(&["system", "df", "--format", "{{json .}}"]) else {
            return;
        };
        let usage = parse_system_df(&df);
        let found = category.candidates(&CancelToken::new()).unwrap();

        for kind in ["Images", "Containers", "Build Cache"] {
            let Some(row) = usage.iter().find(|u| u.kind == kind) else {
                continue;
            };
            if row.reclaimable == 0 {
                continue;
            }
            // Each prune candidate quotes Docker's own reclaimable figure for its kind, so no
            // candidate may claim more than Docker does.
            for c in &found {
                if let ReclaimMethod::ContainerPrune { .. } = c.method {
                    assert!(
                        c.bytes <= usage.iter().map(|u| u.reclaimable).max().unwrap_or(0),
                        "{} claims more than any figure docker reported",
                        c.label
                    );
                }
            }
        }
    }
}
