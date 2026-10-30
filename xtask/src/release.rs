//! Text edits behind `cargo xtask prepare` and `cargo xtask notes`.
//!
//! Every function works on file contents rather than paths, so the release rules are unit
//! tested without touching the repository. Edits keep the rest of each file byte for byte,
//! including CRLF line endings.

use std::fmt;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parses a strict `MAJOR.MINOR.PATCH` version without leading zeros.
    pub fn parse(text: &str) -> Result<Self, String> {
        let invalid = || format!("invalid version {text:?}: expected MAJOR.MINOR.PATCH, for example 0.1.0");
        let mut parts = text.split('.');
        let mut number = || -> Result<u64, String> {
            let part = parts.next().ok_or_else(invalid)?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) || (part.len() > 1 && part.starts_with('0'))
            {
                return Err(invalid());
            }
            part.parse().map_err(|_| invalid())
        };
        let version = Version { major: number()?, minor: number()?, patch: number()? };
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bump {
    Major,
    Minor,
    Patch,
    Exact(Version),
}

impl Bump {
    /// Accepts `major`, `minor`, `patch`, or an exact version with an optional `v` prefix.
    pub fn parse(text: &str) -> Result<Self, String> {
        Ok(match text {
            "major" => Bump::Major,
            "minor" => Bump::Minor,
            "patch" => Bump::Patch,
            _ => Bump::Exact(Version::parse(text.strip_prefix('v').unwrap_or(text))?),
        })
    }

    /// An exact version may equal the current one, so the first release can keep the initial
    /// version. The caller rejects versions that are already tagged.
    pub fn apply(self, current: Version) -> Result<Version, String> {
        let next = |n: u64| n.checked_add(1).ok_or_else(|| format!("cannot bump {current}"));
        Ok(match self {
            Bump::Major => Version { major: next(current.major)?, minor: 0, patch: 0 },
            Bump::Minor => Version { minor: next(current.minor)?, patch: 0, ..current },
            Bump::Patch => Version { patch: next(current.patch)?, ..current },
            Bump::Exact(version) if version >= current => version,
            Bump::Exact(version) => return Err(format!("{version} is lower than the current version {current}")),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Date {
    year: i64,
    month: u32,
    day: u32,
}

impl Date {
    /// Converts days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
    /// `civil_from_days`).
    pub fn from_unix_days(days: i64) -> Self {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        Date { year: yoe + era * 400 + i64::from(month <= 2), month, day }
    }

    /// Parses `YYYY-MM-DD`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let invalid = || format!("invalid date {text:?}: expected YYYY-MM-DD");
        let bytes = text.as_bytes();
        let shape = bytes.len() == 10
            && bytes.iter().enumerate().all(|(i, b)| if i == 4 || i == 7 { *b == b'-' } else { b.is_ascii_digit() });
        if !shape {
            return Err(invalid());
        }
        let year: i64 = text[0..4].parse().map_err(|_| invalid())?;
        let month: u32 = text[5..7].parse().map_err(|_| invalid())?;
        let day: u32 = text[8..10].parse().map_err(|_| invalid())?;
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return Err(invalid());
        }
        Ok(Date { year, month, day })
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// New contents for the files `prepare` changes. `manifest` and `lock` are `None` when the
/// version is unchanged.
#[derive(Debug)]
pub struct Prepared {
    pub version: Version,
    pub manifest: Option<String>,
    pub lock: Option<String>,
    pub changelog: String,
}

pub fn prepare(manifest: &str, lock: &str, changelog: &str, bump: Bump, date: &Date) -> Result<Prepared, String> {
    let current = workspace_version(manifest)?;
    let version = bump.apply(current)?;
    let changelog = release_changelog(changelog, version, date)?;
    let (manifest, lock) = if version == current {
        (None, None)
    } else {
        (Some(set_workspace_version(manifest, version)?), Some(set_lock_versions(lock, current, version)?))
    };
    Ok(Prepared { version, manifest, lock, changelog })
}

/// Reads `version` from the root manifest's `[workspace.package]` table.
pub fn workspace_version(manifest: &str) -> Result<Version, String> {
    workspace_version_span(manifest).map(|(_, version)| version)
}

pub fn set_workspace_version(manifest: &str, version: Version) -> Result<String, String> {
    let (span, _) = workspace_version_span(manifest)?;
    Ok(format!("{}{version}{}", &manifest[..span.start], &manifest[span.end..]))
}

/// Returns the byte range of the quoted version value and the parsed version.
fn workspace_version_span(manifest: &str) -> Result<(Range<usize>, Version), String> {
    let mut in_table = false;
    let mut offset = 0;
    for line in manifest.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let content = line.split('#').next().unwrap_or_default().trim();
        if content.starts_with('[') {
            in_table = content == "[workspace.package]";
            continue;
        }
        let is_version = content.strip_prefix("version").is_some_and(|rest| rest.trim_start().starts_with('='));
        if !in_table || !is_version {
            continue;
        }
        let malformed = || format!("Cargo.toml: cannot read the [workspace.package] version from {:?}", line.trim());
        let equals = line.find('=').ok_or_else(malformed)?;
        let open = equals + line[equals..].find('"').ok_or_else(malformed)? + 1;
        let close = open + line[open..].find('"').ok_or_else(malformed)?;
        let version = Version::parse(&line[open..close])?;
        return Ok((start + open..start + close, version));
    }
    Err("Cargo.toml has no version in [workspace.package]".to_owned())
}

/// Moves the workspace packages in `Cargo.lock` from `from` to `to`. Workspace packages are
/// the `[[package]]` entries without a `source`; Cargo's `--locked` check catches any mismatch.
pub fn set_lock_versions(lock: &str, from: Version, to: Version) -> Result<String, String> {
    let lines: Vec<&str> = lock.split_inclusive('\n').collect();
    let old = format!("version = \"{from}\"");
    let mut out = String::with_capacity(lock.len() + 8);
    let mut changed = 0;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_end() != "[[package]]" {
            out.push_str(lines[i]);
            i += 1;
            continue;
        }
        let end = (i + 1..lines.len()).find(|&j| lines[j].starts_with('[')).unwrap_or(lines.len());
        let workspace = !lines[i..end].iter().any(|line| line.starts_with("source = "));
        for line in &lines[i..end] {
            let content = line.trim_end();
            if workspace && content == old {
                out.push_str(&format!("version = \"{to}\"{}", &line[content.len()..]));
                changed += 1;
            } else {
                out.push_str(line);
            }
        }
        i = end;
    }
    if changed == 0 {
        return Err(format!("Cargo.lock has no workspace package at version {from}"));
    }
    Ok(out)
}

/// Moves the `## [Unreleased]` notes under a new `## [version] - date` heading and leaves an
/// empty `## [Unreleased]` section for the next change.
pub fn release_changelog(changelog: &str, version: Version, date: &Date) -> Result<String, String> {
    let crlf = changelog.contains("\r\n");
    let text = changelog.replace("\r\n", "\n");
    if find_section(&text, &version.to_string()).is_some() {
        return Err(format!("CHANGELOG.md already has a section for {version}"));
    }
    let (heading_end, body) =
        find_section(&text, "Unreleased").ok_or("CHANGELOG.md has no \"## [Unreleased]\" section")?;
    let notes = trim_blank_lines(&text[body.clone()]);
    if notes.is_empty() {
        return Err("CHANGELOG.md has nothing under \"## [Unreleased]\" to release".to_owned());
    }
    let mut out = text[..heading_end].to_owned();
    out.push_str(&format!("\n## [{version}] - {date}\n\n{notes}\n"));
    let rest = &text[body.end..];
    if !rest.is_empty() {
        out.push('\n');
        out.push_str(rest);
    }
    Ok(if crlf { out.replace('\n', "\r\n") } else { out })
}

/// Returns the changelog notes of a released version, with LF line endings.
pub fn release_notes(changelog: &str, version: Version) -> Result<String, String> {
    let text = changelog.replace("\r\n", "\n");
    let (_, body) = find_section(&text, &version.to_string())
        .ok_or_else(|| format!("CHANGELOG.md has no section for {version}"))?;
    let notes = trim_blank_lines(&text[body]);
    if notes.is_empty() {
        return Err(format!("CHANGELOG.md has no notes for {version}"));
    }
    Ok(format!("{notes}\n"))
}

/// Finds a `## [name]` section in LF text. Returns the offset just past its heading line and
/// the byte range of its body, which ends at the next `## ` heading or link reference.
fn find_section(text: &str, name: &str) -> Option<(usize, Range<usize>)> {
    let mut heading_end = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        match heading_end {
            None if is_heading(line, name) => heading_end = Some(offset),
            None => {}
            Some(end) if line.starts_with("## ") || is_link_reference(line) => return Some((end, end..start)),
            Some(_) => {}
        }
    }
    heading_end.map(|end| (end, end..text.len()))
}

fn is_heading(line: &str, name: &str) -> bool {
    line.strip_prefix("## [").and_then(|rest| rest.split_once(']')).is_some_and(|(n, _)| n.eq_ignore_ascii_case(name))
}

fn is_link_reference(line: &str) -> bool {
    line.starts_with('[') && line.contains("]: ")
}

fn trim_blank_lines(text: &str) -> &str {
    let leading: usize = text.split_inclusive('\n').take_while(|line| line.trim().is_empty()).map(str::len).sum();
    text[leading..].trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn date() -> Date {
        Date::parse("2026-11-02").unwrap()
    }

    const MANIFEST: &str = "\
[workspace]
members = [\"crates/a\", \"xtask\"]

[workspace.package]
edition = \"2024\"
version = \"0.1.0\" # released by cargo xtask
license = \"MIT\"

[workspace.dependencies]
version = \"9.9.9\"
";

    const LOCK: &str = "\
# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = \"a\"
version = \"0.1.0\"
dependencies = [
 \"serde\",
]

[[package]]
name = \"serde\"
version = \"0.1.0\"
source = \"registry+https://github.com/rust-lang/crates.io-index\"
checksum = \"abc\"

[[package]]
name = \"xtask\"
version = \"0.1.0\"
";

    const CHANGELOG: &str = "\
# Changelog

Intro.

## [Unreleased]

### Added

- New thing.

## [0.1.0] - 2026-10-01

### Added

- First thing.
";

    #[test]
    fn parses_strict_versions() {
        assert_eq!(v("0.1.0"), Version { major: 0, minor: 1, patch: 0 });
        assert_eq!(v("10.20.30").to_string(), "10.20.30");
        for bad in ["", "1", "1.2", "1.2.3.4", "01.2.3", "1.02.3", "1.2.x", "1.2.3-rc.1", " 1.2.3", "v1.2.3", "1..3"] {
            assert!(Version::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn bumps_versions() {
        let current = v("1.4.7");
        assert_eq!(Bump::parse("patch").unwrap().apply(current).unwrap(), v("1.4.8"));
        assert_eq!(Bump::parse("minor").unwrap().apply(current).unwrap(), v("1.5.0"));
        assert_eq!(Bump::parse("major").unwrap().apply(current).unwrap(), v("2.0.0"));
        assert_eq!(Bump::parse("1.10.0").unwrap().apply(current).unwrap(), v("1.10.0"));
        assert_eq!(Bump::parse("v2.0.0").unwrap().apply(current).unwrap(), v("2.0.0"));
        assert_eq!(Bump::parse("1.4.7").unwrap().apply(current).unwrap(), current);
        assert!(Bump::parse("1.4.6").unwrap().apply(current).unwrap_err().contains("lower"));
        assert!(Bump::parse("Patch").is_err());
        assert!(Bump::Patch.apply(Version { patch: u64::MAX, ..current }).is_err());
    }

    #[test]
    fn converts_unix_days_to_dates() {
        assert_eq!(Date::from_unix_days(0).to_string(), "1970-01-01");
        assert_eq!(Date::from_unix_days(-1).to_string(), "1969-12-31");
        assert_eq!(Date::from_unix_days(11_016).to_string(), "2000-02-29");
        assert_eq!(Date::from_unix_days(19_723).to_string(), "2024-01-01");
        assert_eq!(Date::from_unix_days(19_782).to_string(), "2024-02-29");
        assert_eq!(Date::from_unix_days(20_756).to_string(), "2026-10-30");
    }

    #[test]
    fn parses_dates() {
        assert_eq!(date().to_string(), "2026-11-02");
        assert_eq!(Date::parse("2024-02-29").unwrap(), Date::from_unix_days(19_782));
        for bad in ["2026-1-02", "2026/11/02", "2026-13-01", "2026-00-10", "2026-11-31", "2025-02-29", "+026-11-02"] {
            assert!(Date::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn edits_only_the_workspace_package_version() {
        assert_eq!(workspace_version(MANIFEST).unwrap(), v("0.1.0"));
        let edited = set_workspace_version(MANIFEST, v("0.2.0")).unwrap();
        assert_eq!(edited, MANIFEST.replace("version = \"0.1.0\" #", "version = \"0.2.0\" #"));
        assert!(edited.contains("version = \"9.9.9\""));

        let crlf = MANIFEST.replace('\n', "\r\n");
        assert_eq!(set_workspace_version(&crlf, v("0.2.0")).unwrap(), edited.replace('\n', "\r\n"));
    }

    #[test]
    fn rejects_manifests_without_a_workspace_version() {
        assert!(workspace_version("[package]\nversion = \"0.1.0\"\n").is_err());
        assert!(workspace_version("[workspace.package]\nversion.workspace = true\n").is_err());
        assert!(workspace_version("[workspace.package]\nversion = \"one\"\n").is_err());
    }

    #[test]
    fn bumps_only_workspace_packages_in_the_lock_file() {
        let edited = set_lock_versions(LOCK, v("0.1.0"), v("0.2.0")).unwrap();
        let expected = LOCK
            .replace("name = \"a\"\nversion = \"0.1.0\"", "name = \"a\"\nversion = \"0.2.0\"")
            .replace("name = \"xtask\"\nversion = \"0.1.0\"", "name = \"xtask\"\nversion = \"0.2.0\"");
        assert_eq!(edited, expected);
        assert!(edited.contains("name = \"serde\"\nversion = \"0.1.0\""));

        let crlf = LOCK.replace('\n', "\r\n");
        assert_eq!(set_lock_versions(&crlf, v("0.1.0"), v("0.2.0")).unwrap(), expected.replace('\n', "\r\n"));
        assert!(set_lock_versions(LOCK, v("0.3.0"), v("0.4.0")).is_err());
    }

    #[test]
    fn moves_unreleased_notes_under_the_new_version() {
        let released = release_changelog(CHANGELOG, v("0.2.0"), &date()).unwrap();
        assert_eq!(
            released,
            "\
# Changelog

Intro.

## [Unreleased]

## [0.2.0] - 2026-11-02

### Added

- New thing.

## [0.1.0] - 2026-10-01

### Added

- First thing.
"
        );
        let crlf = release_changelog(&CHANGELOG.replace('\n', "\r\n"), v("0.2.0"), &date()).unwrap();
        assert_eq!(crlf, released.replace('\n', "\r\n"));
    }

    #[test]
    fn releases_the_first_section_and_keeps_link_references() {
        let changelog = "# Changelog\n\n## [Unreleased]\n\n- Only thing.\n\n\n[Unreleased]: https://example.com\n";
        assert_eq!(
            release_changelog(changelog, v("0.1.0"), &date()).unwrap(),
            "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - 2026-11-02\n\n- Only thing.\n\n[Unreleased]: https://example.com\n"
        );
        assert_eq!(
            release_changelog("## [Unreleased]\n- Last line without newline", v("1.0.0"), &date()).unwrap(),
            "## [Unreleased]\n\n## [1.0.0] - 2026-11-02\n\n- Last line without newline\n"
        );
    }

    #[test]
    fn refuses_empty_or_duplicate_releases() {
        let empty = CHANGELOG.replace("### Added\n\n- New thing.\n\n", "");
        assert!(release_changelog(&empty, v("0.2.0"), &date()).unwrap_err().contains("nothing"));
        assert!(release_changelog(CHANGELOG, v("0.1.0"), &date()).unwrap_err().contains("already"));
        assert!(release_changelog("# Changelog\n", v("0.2.0"), &date()).is_err());
    }

    #[test]
    fn extracts_release_notes() {
        assert_eq!(release_notes(CHANGELOG, v("0.1.0")).unwrap(), "### Added\n\n- First thing.\n");
        let released = release_changelog(&CHANGELOG.replace('\n', "\r\n"), v("0.2.0"), &date()).unwrap();
        assert_eq!(release_notes(&released, v("0.2.0")).unwrap(), "### Added\n\n- New thing.\n");
        assert!(release_notes(CHANGELOG, v("0.3.0")).is_err());
        assert!(release_notes("## [0.3.0] - 2026-11-02\n\n", v("0.3.0")).is_err());
    }

    #[test]
    fn prepare_keeps_manifest_and_lock_for_the_current_version() {
        let first =
            prepare(MANIFEST, LOCK, CHANGELOG.replace("0.1.0", "0.0.9").as_str(), Bump::Exact(v("0.1.0")), &date())
                .unwrap();
        assert_eq!(first.version, v("0.1.0"));
        assert!(first.manifest.is_none() && first.lock.is_none());
        assert!(first.changelog.contains("## [0.1.0] - 2026-11-02"));

        let next = prepare(MANIFEST, LOCK, CHANGELOG, Bump::Minor, &date()).unwrap();
        assert_eq!(next.version, v("0.2.0"));
        assert_eq!(workspace_version(next.manifest.as_deref().unwrap()).unwrap(), v("0.2.0"));
        assert!(next.lock.unwrap().contains("name = \"xtask\"\nversion = \"0.2.0\""));
    }

    /// Lock entries as (name, version, has source).
    fn lock_packages(lock: &str) -> Vec<(String, String, bool)> {
        lock.replace("\r\n", "\n")
            .split("[[package]]\n")
            .skip(1)
            .map(|block| {
                let field = |key: &str| {
                    block
                        .lines()
                        .find_map(|line| line.strip_prefix(key))
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_owned()
                };
                (field("name = "), field("version = "), block.contains("\nsource = "))
            })
            .collect()
    }

    #[test]
    fn bumps_the_real_workspace_files() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let lock = fs::read_to_string(root.join("Cargo.lock")).unwrap();
        let current = workspace_version(&manifest).unwrap();
        let next = Bump::Minor.apply(current).unwrap();

        assert_eq!(workspace_version(&set_workspace_version(&manifest, next).unwrap()).unwrap(), next);
        let before = lock_packages(&lock);
        let after = lock_packages(&set_lock_versions(&lock, current, next).unwrap());
        assert_eq!(before.len(), after.len());
        for ((name, old, sourced), (new_name, new, _)) in before.iter().zip(&after) {
            assert_eq!(name, new_name);
            let expected = if *sourced { old.clone() } else { next.to_string() };
            assert_eq!(new, &expected, "{name}");
        }
        let workspace: Vec<&str> = before.iter().filter(|p| !p.2).map(|p| p.0.as_str()).collect();
        assert!(workspace.contains(&"solar-tray") && workspace.contains(&"xtask"), "{workspace:?}");

        let changelog = fs::read_to_string(root.join("CHANGELOG.md")).unwrap().replace("\r\n", "\n");
        assert!(find_section(&changelog, "Unreleased").is_some());
    }
}
