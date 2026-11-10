use super::{Manifest, ManifestVersion};
use eyre::{Context, ContextCompat, Result, bail};
use semver::Version;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Value};

/// Read the version from Cargo.toml
/// Returns None if version field is missing
pub fn read_version(cargo_toml_path: &Path) -> Result<Option<String>> {
    let content =
        fs::read_to_string(cargo_toml_path).context(format!("Failed to read {}", cargo_toml_path.display()))?;
    read_version_from_str(&content)
}

/// Same as `read_version`, but from already-loaded content rather than a path on disk
/// -- lets `git::manifest_version_at` parse a `git show <sha>:Cargo.toml` blob without
/// writing it to a temp file first.
pub fn read_version_from_str(content: &str) -> Result<Option<String>> {
    let doc = content.parse::<DocumentMut>().context("Failed to parse Cargo.toml")?;

    // Try [package] version first
    if let Some(package) = doc.get("package")
        && let Some(version) = package.get("version")
    {
        if let Some(v) = version.as_str() {
            return Ok(Some(v.to_string()));
        }
        // Check for version.workspace = true (inline table or dotted key)
        if is_workspace_version(version) {
            // Version is inherited from workspace, check workspace.package.version
            return read_workspace_version(&doc);
        }
    }

    // Try [workspace.package] version
    if let Some(version) = read_workspace_version(&doc)? {
        return Ok(Some(version));
    }

    Ok(None)
}

/// Check if a version field uses workspace = true (either inline table or dotted key)
fn is_workspace_version(version: &Item) -> bool {
    // Check inline table syntax: version = { workspace = true }
    if let Some(table) = version.as_inline_table()
        && table.get("workspace").is_some_and(|w| w.as_bool() == Some(true))
    {
        return true;
    }
    // Check dotted key syntax: version.workspace = true
    // This gets parsed as a regular table by toml_edit
    if let Some(table) = version.as_table_like()
        && table.get("workspace").is_some_and(|w| w.as_bool() == Some(true))
    {
        return true;
    }
    false
}

/// Read workspace version from [workspace.package]
fn read_workspace_version(doc: &DocumentMut) -> Result<Option<String>> {
    if let Some(workspace) = doc.get("workspace")
        && let Some(package) = workspace.get("package")
        && let Some(version) = package.get("version")
        && let Some(v) = version.as_str()
    {
        return Ok(Some(v.to_string()));
    }
    Ok(None)
}

/// Check if this is a workspace-only manifest (has [workspace] but no [package])
fn is_workspace_only(doc: &DocumentMut) -> bool {
    doc.get("workspace").is_some() && doc.get("package").is_none()
}

/// Update the version in Cargo.toml
/// Creates the version field if it doesn't exist
pub fn write_version(cargo_toml_path: &Path, new_version: &str) -> Result<()> {
    let content =
        fs::read_to_string(cargo_toml_path).context(format!("Failed to read {}", cargo_toml_path.display()))?;

    let mut doc = content.parse::<DocumentMut>().context("Failed to parse Cargo.toml")?;

    // Check if this is a workspace-only manifest (no [package] section)
    if is_workspace_only(&doc) {
        // Update or create [workspace.package].version
        let workspace = doc.get_mut("workspace").context("[workspace] section not found")?;

        if let Item::Table(ws_table) = workspace {
            let package = ws_table
                .entry("package")
                .or_insert(Item::Table(toml_edit::Table::new()));

            if let Item::Table(pkg_table) = package {
                pkg_table["version"] = Item::Value(Value::from(new_version));
            } else {
                bail!("[workspace.package] is not a table");
            }
        } else {
            bail!("[workspace] is not a table");
        }

        fs::write(cargo_toml_path, doc.to_string())
            .context(format!("Failed to write {}", cargo_toml_path.display()))?;
        return Ok(());
    }

    // Check if this is a workspace member with version.workspace = true
    let uses_workspace_version = doc
        .get("package")
        .and_then(|p| p.get("version"))
        .is_some_and(is_workspace_version);

    if uses_workspace_version {
        // Update workspace.package.version instead
        if let Some(workspace) = doc.get_mut("workspace") {
            if let Some(package) = workspace.get_mut("package") {
                if let Some(version) = package.get_mut("version") {
                    *version = Item::Value(Value::from(new_version));
                } else {
                    bail!("Workspace package section exists but has no version field");
                }
            } else {
                bail!("version.workspace = true but no [workspace.package] section found");
            }
        } else {
            bail!("version.workspace = true but no [workspace] section found");
        }
    } else {
        // Update or create [package] version
        let package = doc.entry("package").or_insert(Item::Table(toml_edit::Table::new()));

        if let Item::Table(table) = package {
            table["version"] = Item::Value(Value::from(new_version));
        } else {
            bail!("[package] is not a table");
        }
    }

    fs::write(cargo_toml_path, doc.to_string()).context(format!("Failed to write {}", cargo_toml_path.display()))?;

    Ok(())
}

/// Sync Cargo.lock with Cargo.toml by running cargo update
/// Only runs if Cargo.lock exists (to avoid creating one in library-only projects)
pub fn sync_lockfile(dir: &Path) -> Result<()> {
    let lockfile = dir.join("Cargo.lock");
    if !lockfile.exists() {
        return Ok(());
    }

    // Read Cargo.toml to determine if this is a workspace or a package
    let cargo_toml = dir.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml).context(format!("Failed to read {}", cargo_toml.display()))?;
    let doc = content.parse::<DocumentMut>().context("Failed to parse Cargo.toml")?;

    // Check if this is a workspace-only manifest
    if is_workspace_only(&doc) {
        // For workspaces, just run cargo update to sync all workspace members
        let output = std::process::Command::new("cargo")
            .args(["update", "--workspace"])
            .current_dir(dir)
            .output()
            .context("Failed to run cargo update")?;

        if !output.status.success() {
            bail!(
                "cargo update --workspace failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        return Ok(());
    }

    // For regular packages, get the package name
    let package_name = doc
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .context("Failed to get package name from Cargo.toml")?;

    // Run cargo update -p <package> to sync just this package in the lock file
    let output = std::process::Command::new("cargo")
        .args(["update", "-p", package_name])
        .current_dir(dir)
        .output()
        .context("Failed to run cargo update")?;

    if !output.status.success() {
        bail!("cargo update failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(())
}

/// Check if Cargo.toml exists at the given path
pub fn cargo_toml_exists(dir: &Path) -> bool {
    dir.join("Cargo.toml").exists()
}

/// True when the root `Cargo.toml` exists and declares its own `[package]` table: a
/// binary/library crate root, or a workspace member released standalone. False for a
/// virtual workspace root (`[workspace]` with no `[package]`, e.g. `tatari-tv/marquee`)
/// and for no `Cargo.toml` at all. `cargo install --path .` fails on a virtual
/// manifest, so the release verbs' default install must not offer it there.
pub fn has_package_table(dir: &Path) -> bool {
    let Ok(content) = fs::read_to_string(dir.join("Cargo.toml")) else {
        return false;
    };
    let Ok(doc) = content.parse::<DocumentMut>() else {
        return false;
    };
    doc.get("package").is_some()
}

/// Parse the root `Cargo.toml` when it exists and parses; `None` otherwise.
fn root_manifest(dir: &Path) -> Option<DocumentMut> {
    let content = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    content.parse::<DocumentMut>().ok()
}

/// True when the root crate builds at least one binary: a `[[bin]]` entry, a
/// `src/main.rs`, or a populated `src/bin/`. A library crate (`src/lib.rs` only, e.g.
/// `tatari-tv/renew`) is false: `cargo install --path .` fails on it with
/// `no packages found with binaries`, after the tag is already pushed.
pub fn has_bin_target(dir: &Path) -> bool {
    let Some(doc) = root_manifest(dir) else {
        return false;
    };
    if doc.get("package").is_none() {
        return false;
    }
    if doc
        .get("bin")
        .and_then(Item::as_array_of_tables)
        .is_some_and(|bins| !bins.is_empty())
    {
        return true;
    }
    if dir.join("src/main.rs").is_file() {
        return true;
    }
    fs::read_dir(dir.join("src/bin")).is_ok_and(|mut entries| entries.next().is_some())
}

/// True when the root package depends on `clap`: the fleet's one unambiguous CLI signal.
/// Every CLI (sdv, persona, slack-cli, ...) lists it; no web server (valet, catalog-api,
/// rust-axum-svc, rust-catalog) does. A server has a `src/main.rs` too, but installing it
/// into `~/.cargo/bin` is pointless and `<server> --version` starts the server, so the
/// default install and its verify are for clap binaries only.
pub fn has_clap_dependency(dir: &Path) -> bool {
    root_manifest(dir).is_some_and(|doc| {
        doc.get("package").is_some()
            && doc
                .get("dependencies")
                .and_then(Item::as_table_like)
                .is_some_and(|deps| deps.contains_key("clap"))
    })
}

/// What a post-release install command installs, as far as the verify step can tell.
/// Variants are in the verify step's check order: each one is the first true reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallTarget {
    /// Shell syntax outside quotes (`&&`, `;`, `|`, a redirect, `$X`, `$(...)`, a
    /// backtick, a trailing `&`): verify reads one plain `cargo install`, never a
    /// compound or an expansion.
    ShellSyntax,
    /// Not the one recognized shape, `[NAME=value ...] [env [NAME=value ...]] cargo
    /// [+toolchain] install <args>` (`make install`, a script): nothing says what it
    /// installs.
    NotCargoInstall,
    /// A `cargo install` into a root other than the default (`--root`, or a
    /// `CARGO_INSTALL_ROOT=` / `CARGO_HOME=` prefix): verify would read the default root
    /// and run a stale binary.
    CustomRoot,
    /// A `cargo install` of something other than this checkout: a positional crate name
    /// (crates.io), `--git`, or `--registry`.
    NotThisRepo,
    /// A `--path` that resolves (symlinks followed) outside the repo: the canonical
    /// target.
    OutsideRepo(PathBuf),
    /// A `cargo install` of this checkout: the directory it installs from (`--path <dir>`
    /// or `--path=<dir>` joined onto the repo, else the repo itself).
    Dir(PathBuf),
}

/// `cargo install` options that take a value as the next word (or after `=`). Any other
/// bare word after `cargo install` is a positional crate name.
const CARGO_INSTALL_VALUE_FLAGS: &[&str] = &[
    "--version",
    "--vers",
    "--git",
    "--branch",
    "--tag",
    "--rev",
    "--path",
    "--root",
    "--index",
    "--registry",
    "--bin",
    "--example",
    "--features",
    "-F",
    "--target",
    "--target-dir",
    "--profile",
    "--jobs",
    "-j",
    "--config",
    "-Z",
    "--color",
    "--message-format",
];

/// Characters that make `sh` do more than run one command with literal words when they
/// appear outside quotes and unescaped.
const SHELL_SYNTAX_CHARS: &[char] = &['$', '`', ';', '&', '|', '<', '>', '(', ')', '\n', '*', '?', '['];

/// Characters `sh` treats specially only at the start of a word: `#` opens a comment
/// (`cargo install --path . # --root /x` drops the `--root`), `~` expands to `$HOME`.
const WORD_START_SYNTAX_CHARS: &[char] = &['#', '~'];

/// Environment prefixes that move where `cargo install` puts the binary.
const CARGO_ROOT_VARS: &[&str] = &["CARGO_INSTALL_ROOT", "CARGO_HOME"];

/// An install command split into words, plus whether it met any shell syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandWords {
    words: Vec<String>,
    /// Any unquoted, unescaped `SHELL_SYNTAX_CHARS` or word-initial
    /// `WORD_START_SYNTAX_CHARS`, or a `$` / backtick inside double quotes (where `sh`
    /// still expands them).
    shell_syntax: bool,
}

/// Split `command` into words the way `sh` would for the cases an install line uses:
/// whitespace separates, single quotes are literal, double quotes group (a backslash
/// escapes `"`, `\`, `$` or a backtick inside them), and a backslash outside quotes
/// escapes the next character. Quoted pieces join the word they touch, so
/// `--path="my dir"` is one word. An unterminated quote runs to the end of the command;
/// `sh` itself rejects that line before verify is reached. Shell syntax is reported, not
/// interpreted: `a;b` is still one word, and the caller skips on the flag.
fn split_command_words(command: &str) -> CommandWords {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut shell_syntax = false;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                word.extend(chars.by_ref().take_while(|&q| q != '\''));
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some(e @ ('"' | '\\' | '$' | '`')) => word.push(e),
                            Some(e) => {
                                word.push('\\');
                                word.push(e);
                            }
                            None => word.push('\\'),
                        },
                        '$' | '`' => {
                            shell_syntax = true;
                            word.push(q);
                        }
                        _ => word.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(e) = chars.next() {
                    word.push(e);
                }
            }
            c if SHELL_SYNTAX_CHARS.contains(&c) || (!in_word && WORD_START_SYNTAX_CHARS.contains(&c)) => {
                shell_syntax = true;
                in_word = true;
                word.push(c);
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    CommandWords { words, shell_syntax }
}

/// The `NAME` of a `NAME=value` environment prefix word, `NAME` matching
/// `[A-Za-z_][A-Za-z0-9_]*`; `None` for any other word.
fn assignment_name(word: &str) -> Option<&str> {
    let (name, _) = word.split_once('=')?;
    let mut name_chars = name.chars();
    let first = name_chars.next()?;
    let valid =
        (first.is_ascii_alphabetic() || first == '_') && name_chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some(name)
}

/// Consume leading `NAME=value` words; true when one of them moves the install root.
fn skip_assignments<'a, I: Iterator<Item = &'a str>>(words: &mut std::iter::Peekable<I>) -> bool {
    let mut sets_root = false;
    while let Some(name) = words.peek().and_then(|word| assignment_name(word)) {
        sets_root |= CARGO_ROOT_VARS.contains(&name);
        words.next();
    }
    sets_root
}

/// Resolve what `command` installs, relative to the repo at `dir`, returning the first
/// true reason in `InstallTarget`'s order. Words are split with shell quoting
/// (`split_command_words`); the one recognized shape is `[NAME=value ...] [env
/// [NAME=value ...]] cargo [+toolchain] install <args>`, and anything outside it skips.
/// A `--path` is joined onto `dir`, then both are canonicalized (symlinks followed) for
/// the containment check; a path that does not exist is returned as is, and the binary
/// name check skips it.
pub fn cargo_install_target(dir: &Path, command: &str) -> InstallTarget {
    let split = split_command_words(command);
    if split.shell_syntax {
        return InstallTarget::ShellSyntax;
    }
    let mut words = split.words.iter().map(String::as_str).peekable();
    let mut custom_root = skip_assignments(&mut words);
    if words.next_if_eq(&"env").is_some() {
        custom_root |= skip_assignments(&mut words);
    }
    if words.next() != Some("cargo") {
        return InstallTarget::NotCargoInstall;
    }
    words.next_if(|word| word.starts_with('+'));
    if words.next() != Some("install") {
        return InstallTarget::NotCargoInstall;
    }
    let mut not_this_repo = false;
    let mut path = None;
    while let Some(word) = words.next() {
        let (flag, inline_value) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => (flag, Some(value)),
            _ => (word, None),
        };
        if !flag.starts_with('-') || flag == "--git" || flag == "--registry" {
            not_this_repo = true;
        }
        if flag == "--root" {
            custom_root = true;
        }
        if !CARGO_INSTALL_VALUE_FLAGS.contains(&flag) {
            continue;
        }
        let value = inline_value.or_else(|| words.next());
        if flag == "--path" {
            path = value;
        }
    }
    if custom_root {
        return InstallTarget::CustomRoot;
    }
    if not_this_repo {
        return InstallTarget::NotThisRepo;
    }
    let target = match path {
        Some(p) => dir.join(p),
        None => return InstallTarget::Dir(dir.components().collect()),
    };
    if let (Ok(canonical_target), Ok(canonical_root)) = (target.canonicalize(), dir.canonicalize())
        && !canonical_target.starts_with(&canonical_root)
    {
        return InstallTarget::OutsideRepo(canonical_target);
    }
    // `components()` drops interior `.`: `--path .` names the repo itself, not `<repo>/.`.
    InstallTarget::Dir(target.components().collect())
}

/// The binary the default install produces, for the post-install `--version` check: the
/// first `[[bin]]`'s `name` (falling back to the package name, as cargo does), else the
/// package name when `src/main.rs` exists. `None` for a library crate, a crate whose
/// only binaries live under `src/bin/` (several names, none canonical), a virtual
/// workspace root, or no manifest.
pub fn install_bin_name(dir: &Path) -> Option<String> {
    let doc = root_manifest(dir)?;
    let package_name = doc.get("package")?.get("name")?.as_str()?.to_string();
    if let Some(bins) = doc.get("bin").and_then(Item::as_array_of_tables)
        && let Some(first) = bins.iter().next()
    {
        return Some(
            first
                .get("name")
                .and_then(Item::as_str)
                .map_or(package_name, str::to_string),
        );
    }
    dir.join("src/main.rs").is_file().then_some(package_name)
}

/// Represents a workspace member with an independent version
#[derive(Debug)]
pub struct IndependentVersionMember {
    pub name: String,
    pub path: String,
    pub version: String,
}

/// Check if workspace members have independent versions (not using version.workspace = true)
/// Returns a list of members with independent versions, or empty vec if all use workspace version
pub fn check_workspace_independent_versions(dir: &Path) -> Result<Vec<IndependentVersionMember>> {
    let cargo_toml = dir.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml).context(format!("Failed to read {}", cargo_toml.display()))?;
    let doc = content.parse::<DocumentMut>().context("Failed to parse Cargo.toml")?;

    // Only check if this is a workspace
    let workspace = match doc.get("workspace") {
        Some(ws) => ws,
        None => return Ok(vec![]), // Not a workspace, nothing to check
    };

    // Get workspace members
    let members = match workspace.get("members").and_then(|m| m.as_array()) {
        Some(arr) => arr,
        None => return Ok(vec![]), // No members defined
    };

    let mut independent_versions = Vec::new();

    for member in members.iter() {
        let member_path = match member.as_str() {
            Some(p) => p,
            None => continue,
        };

        let member_cargo_toml = dir.join(member_path).join("Cargo.toml");
        if !member_cargo_toml.exists() {
            continue; // Member might use glob pattern or doesn't exist yet
        }

        let member_content = fs::read_to_string(&member_cargo_toml)
            .context(format!("Failed to read {}", member_cargo_toml.display()))?;
        let member_doc = member_content
            .parse::<DocumentMut>()
            .context(format!("Failed to parse {}", member_cargo_toml.display()))?;

        // Check if this member has an independent version
        if let Some(package) = member_doc.get("package")
            && let Some(version) = package.get("version")
        {
            // Check if it's NOT using workspace = true
            let uses_workspace = is_workspace_version(version);

            if !uses_workspace {
                // This member has an independent version
                if let Some(v) = version.as_str() {
                    let name = package
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or(member_path)
                        .to_string();

                    independent_versions.push(IndependentVersionMember {
                        name,
                        path: member_path.to_string(),
                        version: v.to_string(),
                    });
                }
            }
        }
    }

    Ok(independent_versions)
}

/// Get the path to Cargo.toml in the given directory
pub fn cargo_toml_path(dir: &Path) -> std::path::PathBuf {
    dir.join("Cargo.toml")
}

/// The Cargo manifest adapter. Reuses the free functions above; the workspace
/// independent-version guard remains the cargo adapter's internal concern.
pub struct CargoManifest {
    root: PathBuf,
}

impl CargoManifest {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }
}

impl Manifest for CargoManifest {
    fn path(&self) -> PathBuf {
        cargo_toml_path(&self.root)
    }

    fn read_version(&self) -> Result<ManifestVersion> {
        match read_version(&cargo_toml_path(&self.root))? {
            Some(s) => Ok(ManifestVersion::Static(crate::version::parse_version(&s)?)),
            None => Ok(ManifestVersion::Missing),
        }
    }

    fn write_version(&self, new_version: &Version) -> Result<()> {
        write_version(
            &cargo_toml_path(&self.root),
            &crate::version::format_file_version(new_version),
        )
    }

    fn sync_lockfiles(&self) -> Result<Vec<PathBuf>> {
        sync_lockfile(&self.root)?;
        let lock = self.root.join("Cargo.lock");
        Ok(if lock.exists() { vec![lock] } else { vec![] })
    }

    fn version_files(&self) -> Vec<PathBuf> {
        let mut files = vec![cargo_toml_path(&self.root)];
        let lock = self.root.join("Cargo.lock");
        if lock.exists() {
            files.push(lock);
        }
        files
    }

    fn validate(&self, skip_members: &[String]) -> Result<()> {
        super::validate_project(&self.root, super::ProjectType::Rust, skip_members)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_cargo_toml(dir: &Path, content: &str) -> std::path::PathBuf {
        let path = dir.join("Cargo.toml");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_read_version_package() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[package]
name = "test"
version = "1.2.3"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("1.2.3".to_string()));
    }

    #[test]
    fn test_read_version_missing() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[package]
name = "test"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, None);
    }

    #[test]
    fn test_read_version_workspace() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace.package]
version = "2.0.0"

[package]
name = "test"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("2.0.0".to_string()));
    }

    #[test]
    fn test_write_version() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[package]
name = "test"
version = "1.0.0"
"#,
        );

        write_version(&path, "1.0.1").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("1.0.1".to_string()));
    }

    #[test]
    fn test_write_version_creates_field() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[package]
name = "test"
"#,
        );

        write_version(&path, "0.1.0").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("0.1.0".to_string()));
    }

    #[test]
    fn test_cargo_toml_exists() {
        let dir = TempDir::new().unwrap();
        assert!(!cargo_toml_exists(dir.path()));

        create_cargo_toml(dir.path(), "[package]\nname = \"test\"");
        assert!(cargo_toml_exists(dir.path()));
    }

    #[test]
    fn test_has_package_table() {
        let dir = TempDir::new().unwrap();
        assert!(!has_package_table(dir.path()), "no Cargo.toml at all");

        create_cargo_toml(dir.path(), "[package]\nname = \"test\"\nversion = \"0.1.0\"\n");
        assert!(has_package_table(dir.path()));
    }

    #[test]
    fn test_has_package_table_false_on_virtual_workspace_root() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(dir.path(), "[workspace]\nmembers = [\"crates/a\"]\nresolver = \"2\"\n");
        assert!(!has_package_table(dir.path()));
    }

    #[test]
    fn test_has_bin_target_via_main_rs() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(dir.path(), "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n");
        assert!(!has_bin_target(dir.path()), "no sources yet");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        assert!(!has_bin_target(dir.path()), "library crate");
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        assert!(has_bin_target(dir.path()));
    }

    #[test]
    fn test_has_bin_target_via_bin_table_and_src_bin() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"t\"\npath = \"src/t.rs\"\n",
        );
        assert!(has_bin_target(dir.path()), "[[bin]] declared");

        let dir = TempDir::new().unwrap();
        create_cargo_toml(dir.path(), "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n");
        fs::create_dir_all(dir.path().join("src/bin")).unwrap();
        assert!(!has_bin_target(dir.path()), "empty src/bin");
        fs::write(dir.path().join("src/bin/a.rs"), "fn main() {}").unwrap();
        assert!(has_bin_target(dir.path()), "populated src/bin");
    }

    #[test]
    fn test_has_bin_target_false_on_virtual_workspace_root() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(dir.path(), "[workspace]\nmembers = [\"crates/a\"]\n");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        assert!(!has_bin_target(dir.path()));
    }

    #[test]
    fn test_has_clap_dependency() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            "[package]\nname = \"cli\"\nversion = \"0.1.0\"\n\n[dependencies]\nclap = { version = \"4\", features = [\"derive\"] }\n",
        );
        assert!(has_clap_dependency(dir.path()));

        create_cargo_toml(
            dir.path(),
            "[package]\nname = \"cli\"\nversion = \"0.1.0\"\n\n[dependencies]\nclap = { workspace = true }\n",
        );
        assert!(has_clap_dependency(dir.path()), "workspace-inherited clap");

        create_cargo_toml(
            dir.path(),
            "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n\n[dependencies]\naxum = \"0.8\"\n",
        );
        assert!(!has_clap_dependency(dir.path()), "a web server");

        create_cargo_toml(
            dir.path(),
            "[workspace]\nmembers = [\"cli\"]\n\n[workspace.dependencies]\nclap = \"4\"\n",
        );
        assert!(!has_clap_dependency(dir.path()), "virtual root: the member decides");
        assert!(!has_clap_dependency(TempDir::new().unwrap().path()));
    }

    #[test]
    fn test_cargo_install_target_follows_path() {
        let dir = Path::new("/repo");
        let target = |p: &str| InstallTarget::Dir(PathBuf::from(p));
        assert_eq!(cargo_install_target(dir, "cargo install --path ."), target("/repo"));
        assert_eq!(
            cargo_install_target(dir, "cargo install --locked --path clyde"),
            target("/repo/clyde")
        );
        assert_eq!(
            cargo_install_target(dir, "cargo install --path=clyde --locked"),
            target("/repo/clyde")
        );
        assert_eq!(
            cargo_install_target(dir, "cargo install --features full --path cli"),
            target("/repo/cli"),
            "a flag's value is not a positional crate name"
        );
        assert_eq!(cargo_install_target(dir, "cargo install"), target("/repo"));
        assert_eq!(cargo_install_target(dir, "cargo install --locked"), target("/repo"));
    }

    /// Quoted option values and a `+<toolchain>` are part of a `cargo install` of this repo:
    /// v0.4.6 verified all three, and each resolves to the repo root.
    #[test]
    fn test_cargo_install_target_quotes_and_toolchain() {
        let dir = Path::new("/repo");
        let root = InstallTarget::Dir(PathBuf::from("/repo"));
        for command in [
            "cargo install --path . --config \"build.jobs = 2\"",
            "cargo install --path . --features \"a b\"",
            "cargo +nightly install --path .",
        ] {
            assert_eq!(cargo_install_target(dir, command), root, "{command}");
        }
        assert_eq!(
            cargo_install_target(dir, "cargo install --path \"my cli\""),
            InstallTarget::Dir(PathBuf::from("/repo/my cli"))
        );
        assert_eq!(
            cargo_install_target(dir, "cargo install --path='my cli'"),
            InstallTarget::Dir(PathBuf::from("/repo/my cli"))
        );
        assert_eq!(
            cargo_install_target(dir, "cargo +nightly ripgrep"),
            InstallTarget::NotCargoInstall,
            "a toolchain is not an install subcommand"
        );
    }

    #[test]
    fn test_split_command_words_honors_quotes() {
        let split = |s: &str| split_command_words(s).words;
        assert_eq!(split("a  b\tc"), vec!["a", "b", "c"]);
        assert_eq!(split("--config \"build.jobs = 2\""), vec!["--config", "build.jobs = 2"]);
        assert_eq!(split("--features 'a b'"), vec!["--features", "a b"]);
        assert_eq!(split("--path=\"my dir\"x"), vec!["--path=my dirx"]);
        assert_eq!(split("\"say \\\"hi\\\"\""), vec!["say \"hi\""]);
        assert_eq!(split("a\\ b"), vec!["a b"]);
        assert_eq!(split("''"), vec![""], "an empty quoted word is still a word");
        assert_eq!(split("'unterminated b"), vec!["unterminated b"]);
        assert!(split("   ").is_empty());
    }

    #[test]
    fn test_cargo_install_target_not_cargo_install() {
        let dir = Path::new("/repo");
        assert_eq!(
            cargo_install_target(dir, "make install"),
            InstallTarget::NotCargoInstall
        );
        assert_eq!(
            cargo_install_target(dir, "cargo build --release"),
            InstallTarget::NotCargoInstall
        );
        assert_eq!(cargo_install_target(dir, "cargo"), InstallTarget::NotCargoInstall);
        assert_eq!(cargo_install_target(dir, ""), InstallTarget::NotCargoInstall);
    }

    #[test]
    fn test_cargo_install_target_not_this_repo() {
        let dir = Path::new("/repo");
        for command in [
            "cargo install ripgrep",
            "cargo install --locked ripgrep",
            "cargo install --git https://github.com/x/y",
            "cargo install --git=https://github.com/x/y",
            "cargo install --registry internal tool",
            "cargo install --registry=internal",
        ] {
            assert_eq!(
                cargo_install_target(dir, command),
                InstallTarget::NotThisRepo,
                "{command}"
            );
        }
    }

    /// One row per shape the shakedown-fixes doc's Phase 8 names (plus the three
    /// shakedown commands), against a real repo dir with a sibling `other` crate and an
    /// in-repo symlink pointing at it, so containment is checked on canonical paths.
    #[test]
    fn test_cargo_install_target_shape_table() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let other = tmp.path().join("other");
        fs::create_dir_all(repo.join("cli")).unwrap();
        fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(&other, repo.join("elsewhere")).unwrap();
        let other_canonical = other.canonicalize().unwrap();
        let root = || InstallTarget::Dir(repo.clone());
        let abs_cli = format!("cargo install --path {}", repo.join("cli").display());
        let rows: Vec<(&str, InstallTarget)> = vec![
            (r"cargo install --path . --features a\;b", root()),
            ("cargo install --path . --features \"a&&b\"", root()),
            ("cargo install --path . 2>&1", InstallTarget::ShellSyntax),
            ("cargo install --path $(pwd)", InstallTarget::ShellSyntax),
            ("cargo install --path $X", InstallTarget::ShellSyntax),
            ("cargo install --path \"$X\"", InstallTarget::ShellSyntax),
            ("cargo install --path `pwd`", InstallTarget::ShellSyntax),
            ("cargo install --path . &", InstallTarget::ShellSyntax),
            ("cargo install --path . && echo done", InstallTarget::ShellSyntax),
            ("cargo install --path .;", InstallTarget::ShellSyntax),
            ("cargo install --path . | tee log", InstallTarget::ShellSyntax),
            ("(cargo install --path .)", InstallTarget::ShellSyntax),
            ("cargo install --path . > log", InstallTarget::ShellSyntax),
            ("cargo install --path ~/x", InstallTarget::ShellSyntax),
            ("cargo install --path ./sr*", InstallTarget::ShellSyntax),
            ("cargo install --path . # c", InstallTarget::ShellSyntax),
            ("cargo install --path . # --root /x", InstallTarget::ShellSyntax),
            ("cargo install --path \"~/x\"", InstallTarget::Dir(repo.join("~/x"))),
            ("cargo install --path '*'", InstallTarget::Dir(repo.join("*"))),
            (
                "cargo install --path .\ncargo install ripgrep",
                InstallTarget::ShellSyntax,
            ),
            ("env A=1 cargo install --path .", root()),
            ("env cargo install --path .", root()),
            ("RUSTFLAGS=-g cargo install --path .", root()),
            ("A=1 env B=2 cargo +nightly install", root()),
            ("RUSTFLAGS=\"-C debuginfo=2\" cargo install", root()),
            (
                "CARGO_INSTALL_ROOT=/x cargo install --path .",
                InstallTarget::CustomRoot,
            ),
            ("env CARGO_HOME=/x cargo install --path .", InstallTarget::CustomRoot),
            ("cargo install --path . --root /x", InstallTarget::CustomRoot),
            ("cargo install --root=/x", InstallTarget::CustomRoot),
            (
                "cargo install --path ../other",
                InstallTarget::OutsideRepo(other_canonical.clone()),
            ),
            (
                "cargo install --path elsewhere",
                InstallTarget::OutsideRepo(other_canonical.clone()),
            ),
            (&abs_cli, InstallTarget::Dir(repo.join("cli"))),
            ("cargo install --path missing", InstallTarget::Dir(repo.join("missing"))),
            ("1A=1 cargo install --path .", InstallTarget::NotCargoInstall),
            ("env -i cargo install --path .", InstallTarget::NotCargoInstall),
            ("env", InstallTarget::NotCargoInstall),
        ];
        for (command, expected) in rows {
            assert_eq!(cargo_install_target(&repo, command), expected, "{command:?}");
        }
    }

    /// The skip reasons are checked in the doc's order: each row would also match a later
    /// reason, and the earlier one wins.
    #[test]
    fn test_cargo_install_target_check_order() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(tmp.path().join("other")).unwrap();
        for (command, expected) in [
            ("make install; true", InstallTarget::ShellSyntax),
            ("CARGO_HOME=/x make install", InstallTarget::NotCargoInstall),
            ("CARGO_HOME=/x cargo install ripgrep", InstallTarget::CustomRoot),
            ("cargo install --root /x --path ../other", InstallTarget::CustomRoot),
            (
                "cargo install --git https://x/y --path ../other",
                InstallTarget::NotThisRepo,
            ),
            ("cargo install ripgrep --path ../other", InstallTarget::NotThisRepo),
        ] {
            assert_eq!(cargo_install_target(&repo, command), expected, "{command:?}");
        }
    }

    #[test]
    fn test_split_command_words_reports_shell_syntax() {
        let syntax = |s: &str| split_command_words(s).shell_syntax;
        for command in [
            "a && b", "a;", "a | b", "a 2>&1", "$X", "\"$X\"", "`a`", "\"`a`\"", "a &", "(a)", "a\nb", "a # c", "#c",
            "~/x", "a ~", "a*", "a?", "[ab]",
        ] {
            assert!(syntax(command), "{command:?}");
        }
        for command in [
            r"a\;b",
            "'a && b'",
            "\"a&&b\"",
            "\"a;b|c\"",
            "'$X'",
            "\"\\$X\"",
            "a\\$b",
            "a b",
            "a#b",
            "a~b",
            "\"~/x\"",
            "'~/x'",
            "\\~/x",
            "'*'",
            "\"a*?[\"",
            "\\*",
            "\"#\"",
        ] {
            assert!(!syntax(command), "{command:?}");
        }
        assert_eq!(split_command_words("\"\\$X\"").words, vec!["$X"]);
        assert_eq!(split_command_words("a;b").words, vec!["a;b"], "reported, not split");
    }

    #[test]
    fn test_assignment_name() {
        assert_eq!(assignment_name("RUSTFLAGS=-g"), Some("RUSTFLAGS"));
        assert_eq!(assignment_name("_A1="), Some("_A1"));
        assert_eq!(assignment_name("A=b=c"), Some("A"));
        assert_eq!(assignment_name("1A=b"), None);
        assert_eq!(assignment_name("=b"), None);
        assert_eq!(assignment_name("A-B=c"), None);
        assert_eq!(assignment_name("--path=."), None);
        assert_eq!(assignment_name("cargo"), None);
    }

    #[test]
    fn test_install_bin_name_prefers_bin_table_then_package_name() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            "[package]\nname = \"tool-rs\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"tool\"\npath = \"src/main.rs\"\n",
        );
        assert_eq!(install_bin_name(dir.path()).as_deref(), Some("tool"));

        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            "[package]\nname = \"tool-rs\"\nversion = \"0.1.0\"\n\n[[bin]]\npath = \"src/main.rs\"\n",
        );
        assert_eq!(
            install_bin_name(dir.path()).as_deref(),
            Some("tool-rs"),
            "[[bin]] without a name takes the package name"
        );

        let dir = TempDir::new().unwrap();
        create_cargo_toml(dir.path(), "[package]\nname = \"tool-rs\"\nversion = \"0.1.0\"\n");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        assert_eq!(install_bin_name(dir.path()).as_deref(), Some("tool-rs"));
    }

    #[test]
    fn test_install_bin_name_none_for_library_src_bin_only_and_workspace_root() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(dir.path(), "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        assert_eq!(install_bin_name(dir.path()), None, "library crate");

        fs::create_dir_all(dir.path().join("src/bin")).unwrap();
        fs::write(dir.path().join("src/bin/a.rs"), "fn main() {}").unwrap();
        assert_eq!(install_bin_name(dir.path()), None, "src/bin only: no canonical name");

        let dir = TempDir::new().unwrap();
        create_cargo_toml(dir.path(), "[workspace]\nmembers = [\"crates/a\"]\n");
        assert_eq!(install_bin_name(dir.path()), None);
        assert_eq!(install_bin_name(TempDir::new().unwrap().path()), None, "no manifest");
    }

    #[test]
    fn test_read_version_workspace_only() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.package]
version = "3.0.0"

[workspace.dependencies]
serde = "1.0"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("3.0.0".to_string()));
    }

    #[test]
    fn test_read_version_workspace_only_no_version() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.dependencies]
serde = "1.0"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, None);
    }

    #[test]
    fn test_write_version_workspace_only_creates_workspace_package() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.dependencies]
serde = "1.0"
"#,
        );

        write_version(&path, "0.2.0").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("0.2.0".to_string()));

        // Verify that no [package] section was created
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("[package]"), "Should not create [package] section");
        assert!(
            content.contains("[workspace.package]"),
            "Should create [workspace.package] section"
        );
    }

    #[test]
    fn test_write_version_workspace_only_updates_existing() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.package]
version = "1.0.0"
edition = "2021"

[workspace.dependencies]
serde = "1.0"
"#,
        );

        write_version(&path, "1.1.0").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("1.1.0".to_string()));

        // Verify the content still has no [package] section
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("\n[package]"), "Should not create [package] section");
    }

    // Tests for check_workspace_independent_versions

    fn create_member_cargo_toml(dir: &Path, member_path: &str, content: &str) {
        let member_dir = dir.join(member_path);
        fs::create_dir_all(&member_dir).unwrap();
        let path = member_dir.join("Cargo.toml");
        fs::write(&path, content).unwrap();
    }

    #[test]
    fn test_check_independent_versions_not_a_workspace() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[package]
name = "my-app"
version = "1.0.0"
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert!(result.is_empty(), "Non-workspace should return empty vec");
    }

    #[test]
    fn test_check_independent_versions_workspace_no_members() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]

[workspace.package]
version = "1.0.0"
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert!(result.is_empty(), "Workspace with no members should return empty vec");
    }

    #[test]
    fn test_check_independent_versions_all_use_workspace_version() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.package]
version = "1.0.0"
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crate-a",
            r#"
[package]
name = "crate-a"
version.workspace = true
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crate-b",
            r#"
[package]
name = "crate-b"
version.workspace = true
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert!(
            result.is_empty(),
            "All members using workspace version should return empty vec"
        );
    }

    #[test]
    fn test_check_independent_versions_some_independent() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b", "crate-c"]

[workspace.package]
version = "1.0.0"
"#,
        );

        // crate-a uses workspace version
        create_member_cargo_toml(
            dir.path(),
            "crate-a",
            r#"
[package]
name = "crate-a"
version.workspace = true
"#,
        );

        // crate-b has independent version
        create_member_cargo_toml(
            dir.path(),
            "crate-b",
            r#"
[package]
name = "crate-b"
version = "2.0.0"
"#,
        );

        // crate-c has independent version
        create_member_cargo_toml(
            dir.path(),
            "crate-c",
            r#"
[package]
name = "crate-c"
version = "3.5.0"
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert_eq!(result.len(), 2, "Should detect 2 members with independent versions");

        let names: Vec<&str> = result.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"crate-b"), "Should detect crate-b");
        assert!(names.contains(&"crate-c"), "Should detect crate-c");

        let crate_b = result.iter().find(|m| m.name == "crate-b").unwrap();
        assert_eq!(crate_b.version, "2.0.0");
        assert_eq!(crate_b.path, "crate-b");

        let crate_c = result.iter().find(|m| m.name == "crate-c").unwrap();
        assert_eq!(crate_c.version, "3.5.0");
        assert_eq!(crate_c.path, "crate-c");
    }

    #[test]
    fn test_check_independent_versions_all_independent() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crate-a",
            r#"
[package]
name = "crate-a"
version = "1.0.0"
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crate-b",
            r#"
[package]
name = "crate-b"
version = "2.0.0"
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert_eq!(result.len(), 2, "Should detect both members with independent versions");
    }

    #[test]
    fn test_check_independent_versions_member_missing_cargo_toml() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-missing"]

[workspace.package]
version = "1.0.0"
"#,
        );

        // Only create crate-a, crate-missing doesn't exist
        create_member_cargo_toml(
            dir.path(),
            "crate-a",
            r#"
[package]
name = "crate-a"
version = "1.0.0"
"#,
        );

        // Should not error, just skip missing member
        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert_eq!(result.len(), 1, "Should only detect existing member");
        assert_eq!(result[0].name, "crate-a");
    }

    #[test]
    fn test_check_independent_versions_member_no_version_field() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a"]

[workspace.package]
version = "1.0.0"
"#,
        );

        // Member has no version field at all
        create_member_cargo_toml(
            dir.path(),
            "crate-a",
            r#"
[package]
name = "crate-a"
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert!(result.is_empty(), "Member with no version field should not be flagged");
    }

    #[test]
    fn test_check_independent_versions_nested_path() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crates/core", "crates/cli"]

[workspace.package]
version = "1.0.0"
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crates/core",
            r#"
[package]
name = "my-core"
version = "0.5.0"
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crates/cli",
            r#"
[package]
name = "my-cli"
version.workspace = true
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "my-core");
        assert_eq!(result[0].path, "crates/core");
        assert_eq!(result[0].version, "0.5.0");
    }

    /// PRE-EXISTING BEHAVIOR, NOT FIXED HERE (design doc risk table: "Workspace glob
    /// members (`crates/*`) silently skipped by independent-version scan,
    /// cargo.rs:234-236"). A `members` entry is treated as a literal path
    /// (`dir.join(member_path).join("Cargo.toml")`); a glob pattern like `crates/*`
    /// never resolves to a real file, so the `!member_cargo_toml.exists()` guard
    /// silently `continue`s past it -- an independent version living behind a glob
    /// member is invisible to this scan, even though a real Cargo workspace resolves
    /// the glob to real member crates. This test documents that gap; it is NOT a
    /// regression test for a fix (no fix lands in this phase).
    #[test]
    fn test_check_independent_versions_glob_member_silently_skipped() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crates/*"]

[workspace.package]
version = "1.0.0"
"#,
        );

        // A real member crate that Cargo's own glob expansion would pick up, with an
        // independent (non-workspace) version -- exactly the case the guard exists to
        // catch when the member is named explicitly.
        create_member_cargo_toml(
            dir.path(),
            "crates/pinned",
            r#"
[package]
name = "pinned-crate"
version = "9.9.9"
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert!(
            result.is_empty(),
            "documents the known gap: a glob members entry is never expanded, so \
             pinned-crate's independent version is invisible to the scan (got: {result:?})"
        );
    }

    // Tests for dotted key syntax: version.workspace = true

    #[test]
    fn test_read_version_dotted_workspace_syntax() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace.package]
version = "2.5.0"

[package]
name = "test"
version.workspace = true
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(
            version,
            Some("2.5.0".to_string()),
            "Should read workspace version when using dotted key syntax"
        );
    }

    #[test]
    fn test_read_version_inline_table_workspace_syntax() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace.package]
version = "3.0.0"

[package]
name = "test"
version = { workspace = true }
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(
            version,
            Some("3.0.0".to_string()),
            "Should read workspace version when using inline table syntax"
        );
    }

    #[test]
    fn test_write_version_preserves_dotted_workspace_syntax() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = []

[workspace.package]
version = "1.0.0"
edition = "2021"

[package]
name = "test"
version.workspace = true
edition.workspace = true
"#,
        );

        write_version(&path, "1.1.0").unwrap();

        // Should update workspace.package.version, not [package].version
        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("1.1.0".to_string()));

        // Verify the file still uses workspace syntax (not a direct version)
        let content = fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("version.workspace = true") || content.contains("version = { workspace = true }"),
            "Should preserve workspace version inheritance in [package]"
        );
        // The workspace.package.version will have "1.1.0", but [package] section should NOT
        // have version = "1.1.0" on its own line
        let package_section_start = content.find("[package]").unwrap();
        let package_section = &content[package_section_start..];
        assert!(
            !package_section.contains("\nversion = \"1.1.0\""),
            "Should NOT write direct version to [package] section"
        );
    }

    #[test]
    fn test_write_version_preserves_inline_table_workspace_syntax() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = []

[workspace.package]
version = "1.0.0"

[package]
name = "test"
version = { workspace = true }
"#,
        );

        write_version(&path, "1.2.0").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("1.2.0".to_string()));

        let content = fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("version = { workspace = true }"),
            "Should preserve inline table workspace syntax"
        );
    }

    #[test]
    fn test_write_version_direct_version_not_workspace() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[package]
name = "standalone"
version = "1.0.0"
"#,
        );

        write_version(&path, "1.0.1").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("1.0.1".to_string()));

        // Should update direct version
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("version = \"1.0.1\""), "Should update direct version");
    }

    #[test]
    fn test_is_workspace_version_dotted_key() {
        let content = r#"
[package]
name = "test"
version.workspace = true
"#;
        let doc = content.parse::<DocumentMut>().unwrap();
        let version = doc.get("package").unwrap().get("version").unwrap();
        assert!(is_workspace_version(version), "Should detect dotted key syntax");
    }

    #[test]
    fn test_is_workspace_version_inline_table() {
        let content = r#"
[package]
name = "test"
version = { workspace = true }
"#;
        let doc = content.parse::<DocumentMut>().unwrap();
        let version = doc.get("package").unwrap().get("version").unwrap();
        assert!(is_workspace_version(version), "Should detect inline table syntax");
    }

    #[test]
    fn test_is_workspace_version_direct_string() {
        let content = r#"
[package]
name = "test"
version = "1.0.0"
"#;
        let doc = content.parse::<DocumentMut>().unwrap();
        let version = doc.get("package").unwrap().get("version").unwrap();
        assert!(
            !is_workspace_version(version),
            "Direct version string should NOT be workspace version"
        );
    }

    #[test]
    fn test_is_workspace_version_false_value() {
        let content = r#"
[package]
name = "test"
version.workspace = false
"#;
        let doc = content.parse::<DocumentMut>().unwrap();
        let version = doc.get("package").unwrap().get("version").unwrap();
        assert!(
            !is_workspace_version(version),
            "workspace = false should NOT be detected as workspace version"
        );
    }
}
