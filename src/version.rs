use eyre::{Result, bail};
use semver::Version;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BumpType {
    Major,
    Minor,
    #[default]
    Patch,
}

impl BumpType {
    pub fn from_cli(major: bool, minor: bool) -> Self {
        match (major, minor) {
            (true, _) => BumpType::Major,
            (_, true) => BumpType::Minor,
            _ => BumpType::Patch,
        }
    }
}

/// Parse a version string into a semver Version
pub fn parse_version(version_str: &str) -> Result<Version> {
    let version_str = version_str.strip_prefix('v').unwrap_or(version_str);
    let version = Version::parse(version_str)?;

    // Error if pre-release or build metadata present
    if !version.pre.is_empty() {
        bail!("Pre-release versions are not supported: {}", version_str);
    }
    if !version.build.is_empty() {
        bail!("Build metadata versions are not supported: {}", version_str);
    }

    Ok(version)
}

/// Bump a version according to the bump type
pub fn bump_version(version: &Version, bump_type: BumpType) -> Version {
    let mut new_version = version.clone();

    match bump_type {
        BumpType::Major => {
            new_version.major += 1;
            new_version.minor = 0;
            new_version.patch = 0;
        }
        BumpType::Minor => {
            new_version.minor += 1;
            new_version.patch = 0;
        }
        BumpType::Patch => {
            new_version.patch += 1;
        }
    }

    new_version
}

/// Format version for Cargo.toml (no 'v' prefix)
pub fn format_cargo_version(version: &Version) -> String {
    format!("{}.{}.{}", version.major, version.minor, version.patch)
}

/// Format version for git tag (with 'v' prefix)
pub fn format_tag(version: &Version) -> String {
    format!("v{}.{}.{}", version.major, version.minor, version.patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bump_type_from_cli() {
        assert_eq!(BumpType::from_cli(false, false), BumpType::Patch);
        assert_eq!(BumpType::from_cli(true, false), BumpType::Major);
        assert_eq!(BumpType::from_cli(false, true), BumpType::Minor);
        assert_eq!(BumpType::from_cli(true, true), BumpType::Major); // major takes precedence
    }

    #[test]
    fn test_parse_version() {
        let v = parse_version("1.2.3").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
    }

    #[test]
    fn test_parse_version_with_v_prefix() {
        let v = parse_version("v1.2.3").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
    }

    #[test]
    fn test_parse_version_prerelease_error() {
        let result = parse_version("1.0.0-alpha");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_version_build_metadata_error() {
        let result = parse_version("1.0.0+build123");
        assert!(result.is_err());
    }

    #[test]
    fn test_bump_patch() {
        let v = Version::new(1, 2, 3);
        let bumped = bump_version(&v, BumpType::Patch);
        assert_eq!(bumped, Version::new(1, 2, 4));
    }

    #[test]
    fn test_bump_patch_rollover() {
        let v = Version::new(1, 2, 9);
        let bumped = bump_version(&v, BumpType::Patch);
        assert_eq!(bumped, Version::new(1, 2, 10));

        let v = Version::new(1, 2, 99);
        let bumped = bump_version(&v, BumpType::Patch);
        assert_eq!(bumped, Version::new(1, 2, 100));
    }

    #[test]
    fn test_bump_minor() {
        let v = Version::new(1, 2, 3);
        let bumped = bump_version(&v, BumpType::Minor);
        assert_eq!(bumped, Version::new(1, 3, 0));
    }

    #[test]
    fn test_bump_major() {
        let v = Version::new(1, 2, 3);
        let bumped = bump_version(&v, BumpType::Major);
        assert_eq!(bumped, Version::new(2, 0, 0));
    }

    #[test]
    fn test_format_cargo_version() {
        let v = Version::new(1, 2, 3);
        assert_eq!(format_cargo_version(&v), "1.2.3");
    }

    #[test]
    fn test_format_tag() {
        let v = Version::new(1, 2, 3);
        assert_eq!(format_tag(&v), "v1.2.3");
    }
}
