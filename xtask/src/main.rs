//! `cargo xtask`: release tooling used by `.github/workflows/release.yml`.

mod release;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use release::{Bump, Date, Version};

const USAGE: &str = "\
usage:
  cargo xtask prepare <patch|minor|major|X.Y.Z> [--date YYYY-MM-DD]
      Set the workspace version in Cargo.toml and Cargo.lock and move the CHANGELOG.md
      [Unreleased] notes under it. X.Y.Z may equal the current version while it is untagged.
      Prints the new version.
  cargo xtask notes <X.Y.Z> <output-file>
      Write the CHANGELOG.md notes of a version to a file.";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args.iter().map(String::as_str).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[&str]) -> Result<(), String> {
    match args {
        ["prepare", bump] => prepare(Bump::parse(bump)?, today()),
        ["prepare", bump, "--date", date] => prepare(Bump::parse(bump)?, Date::parse(date)?),
        ["notes", version, output] => notes(Version::parse(version)?, Path::new(output)),
        _ => Err(USAGE.to_owned()),
    }
}

fn prepare(bump: Bump, date: Date) -> Result<(), String> {
    let root = workspace_root();
    let prepared = release::prepare(
        &read(&root, "Cargo.toml")?,
        &read(&root, "Cargo.lock")?,
        &read(&root, "CHANGELOG.md")?,
        bump,
        &date,
    )?;
    let tag = format!("v{}", prepared.version);
    if tag_exists(&root, &tag)? {
        return Err(format!("tag {tag} already exists"));
    }
    if let Some(manifest) = &prepared.manifest {
        write(&root, "Cargo.toml", manifest)?;
    }
    if let Some(lock) = &prepared.lock {
        write(&root, "Cargo.lock", lock)?;
    }
    write(&root, "CHANGELOG.md", &prepared.changelog)?;
    eprintln!("Prepared {tag} ({date}). Review the changes, then commit and tag them.");
    println!("{}", prepared.version);
    Ok(())
}

fn notes(version: Version, output: &Path) -> Result<(), String> {
    let notes = release::release_notes(&read(&workspace_root(), "CHANGELOG.md")?, version)?;
    fs::write(output, notes).map_err(|e| format!("cannot write {}: {e}", output.display()))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives in the workspace root").to_owned()
}

fn read(root: &Path, name: &str) -> Result<String, String> {
    fs::read_to_string(root.join(name)).map_err(|e| format!("cannot read {name}: {e}"))
}

fn write(root: &Path, name: &str, contents: &str) -> Result<(), String> {
    fs::write(root.join(name), contents).map_err(|e| format!("cannot write {name}: {e}"))
}

fn tag_exists(root: &Path, tag: &str) -> Result<bool, String> {
    let status = Command::new("git")
        .args(["rev-parse", "--quiet", "--verify", &format!("refs/tags/{tag}")])
        .current_dir(root)
        .stdout(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run git to check for tag {tag}: {e}"))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!("git could not check for tag {tag} ({status})")),
    }
}

fn today() -> Date {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    Date::from_unix_days(i64::try_from(seconds / 86_400).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_or_malformed_commands() {
        let cases: [&[&str]; 5] =
            [&[], &["publish"], &["prepare"], &["prepare", "patch", "--date"], &["notes", "0.1.0"]];
        for args in cases {
            assert_eq!(run(args).unwrap_err(), USAGE, "{args:?}");
        }
        assert!(run(&["prepare", "1.2"]).unwrap_err().contains("invalid version"));
        assert!(run(&["prepare", "patch", "--date", "2026-02-30"]).unwrap_err().contains("invalid date"));
        assert!(run(&["notes", "next", "notes.md"]).unwrap_err().contains("invalid version"));
    }

    #[test]
    fn writes_notes_of_a_released_version_only() {
        let output = env::temp_dir().join(format!("xtask-notes-{}.md", std::process::id()));
        assert!(run(&["notes", "999.0.0", output.to_str().unwrap()]).unwrap_err().contains("no section"));
        assert!(!output.exists());
    }
}
