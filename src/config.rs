//! Repo-local facts config: `bump.yml` at the repo ROOT.
//!
//! Deliberate, doc-sanctioned exception to `rules/rust.md`'s "never load config from
//! the current working directory" rule: this is a repo-COMMITTED facts file (skip
//! members, the install command), same trust model as `.otto.yml` -- you already run
//! that repo's build tooling. It is loaded from the root of the directory bump is
//! processing (`<dir>/bump.yml`), NOT from XDG. A missing file is not an error: it
//! means all defaults.
//!
//! Facts only, never flows: the two release flows (gated | ungated) stay hard-coded in
//! `src/release.rs` (later phases). This file carries `skip-members` and `install`.

use eyre::{Context, Result};
use log::{debug, info};
use std::fs;
use std::path::Path;

/// The repo-root config file name (never XDG -- see module docs).
pub const CONFIG_FILE_NAME: &str = "bump.yml";

/// Repo-local facts. Unknown keys are a loud error naming the offending key
/// (`deny_unknown_fields`) -- a typo in `bump.yml` must never silently no-op.
#[derive(Debug, Default, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    /// Workspace members with an independent (literal) version, matched by package
    /// name. Same contract as the `--skip-member` CLI flag; the flag overrides this
    /// when provided (see `effective_skip_members`).
    #[serde(default)]
    pub skip_members: Vec<String>,

    /// Command run by `bump release`/`bump finish` (Phase 5-7) after a successful
    /// release. `None` = the verb's own default install step; this phase only loads
    /// and exposes the fact, it does not invent release-verb behavior.
    #[serde(default)]
    pub install: Option<String>,
}

/// Load `bump.yml` from the ROOT of `dir` (the directory bump is processing, not XDG).
/// A missing file is fine -- returns `Config::default()`, not an error. Logs which
/// file actually loaded, or that none was found.
pub fn load(dir: &Path) -> Result<Config> {
    debug!("load: dir={}", dir.display());
    let path = dir.join(CONFIG_FILE_NAME);
    if !path.exists() {
        info!(
            "load: no {} found under {} (using defaults)",
            CONFIG_FILE_NAME,
            dir.display()
        );
        return Ok(Config::default());
    }

    let contents = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    // Embed the serde_yaml error's own Display (which names the offending key for
    // deny_unknown_fields) directly in the top-level message rather than only in the
    // eyre chain, so callers reading `err.to_string()` see it without unwrapping a
    // Debug chain.
    let config: Config =
        serde_yaml::from_str(&contents).map_err(|e| eyre::eyre!("failed to parse {}: {e}", path.display()))?;
    info!("load: loaded config from {}", path.display());
    debug!(
        "load: skip_members={:?} install={:?}",
        config.skip_members, config.install
    );
    Ok(config)
}

/// Precedence: CLI flags > config file > defaults (general.md). The `--skip-member`
/// flag overrides the config's `skip-members` wholesale when provided (any value,
/// since the flag requires at least one argument once given); otherwise the config's
/// list is used; otherwise empty.
pub fn effective_skip_members(cli_skip_member: &[String], config: &Config) -> Vec<String> {
    debug!(
        "effective_skip_members: cli_skip_member={:?} config.skip_members={:?}",
        cli_skip_member, config.skip_members
    );
    if !cli_skip_member.is_empty() {
        cli_skip_member.to_vec()
    } else {
        config.skip_members.clone()
    }
}

#[cfg(test)]
mod tests;
