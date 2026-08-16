//! Repository build-system detection (spec §3.0 M3 language forms).
//!
//! One Rust-core repo produces many artifacts: the M3 verification form and
//! the api_compat tool both depend on WHAT the repo is built with. Detection
//! is conservative and fail-open: an unknown project is honestly `unknown`
//! in every downstream verdict, never a fake pass.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The build system driving this repository (M3 language forms).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectKind {
    Rust,
    Gradle,
    SwiftPm,
    Xcode,
    Unknown,
}

impl ProjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectKind::Rust => "rust",
            ProjectKind::Gradle => "gradle",
            ProjectKind::SwiftPm => "swiftpm",
            ProjectKind::Xcode => "xcode",
            ProjectKind::Unknown => "unknown",
        }
    }
}

/// Detect the repository's build system from its root manifests.
pub fn detect(repo: &Path) -> ProjectKind {
    if repo.join("Cargo.toml").is_file() {
        return ProjectKind::Rust;
    }
    if repo.join("gradlew").is_file()
        || repo.join("build.gradle.kts").is_file()
        || repo.join("build.gradle").is_file()
        || repo.join("settings.gradle.kts").is_file()
        || repo.join("settings.gradle").is_file()
    {
        return ProjectKind::Gradle;
    }
    if repo.join("Package.swift").is_file() {
        return ProjectKind::SwiftPm;
    }
    let has_xcode = std::fs::read_dir(repo)
        .map(|rd| {
            rd.flatten().any(|e| {
                let p = e.path();
                p.extension()
                    .is_some_and(|x| x == "xcodeproj" || x == "xcworkspace")
            })
        })
        .unwrap_or(false);
    if has_xcode {
        return ProjectKind::Xcode;
    }
    ProjectKind::Unknown
}

/// The shipped default inner-loop lint command (M3, no Docker). An explicit
/// `.ward/config.toml` `lint.command` always wins; this only fills the
/// shipped-default slot.
pub const DEFAULT_LINT_RUST: &str = "cargo check --quiet";

/// Shipped defaults for the outer-loop sandbox (M3). Explicit config always
/// wins; these only fill the shipped-default slot.
pub const DEFAULT_VERIFY_RUST: &str = "cargo test --quiet";
pub const DEFAULT_IMAGE_RUST: &str = "rust:1-bookworm";

/// Default lint precheck for a project kind; `""` means "no deterministic
/// precheck available" (the run reports that honestly instead of guessing).
pub fn default_lint(kind: ProjectKind) -> &'static str {
    match kind {
        ProjectKind::Rust => DEFAULT_LINT_RUST,
        ProjectKind::Gradle => "./gradlew classes",
        ProjectKind::SwiftPm => "swift build",
        // Xcode needs code signing / a host macOS toolchain — the inner
        // precheck honestly defers instead of half-running a build.
        ProjectKind::Xcode | ProjectKind::Unknown => "",
    }
}

/// Default outer-loop (verify_command, docker_image) for a project kind.
/// `("", "")` means "no deterministic sandbox form" → honest `unknown`.
pub fn default_verify(kind: ProjectKind) -> (&'static str, &'static str) {
    match kind {
        ProjectKind::Rust => (DEFAULT_VERIFY_RUST, DEFAULT_IMAGE_RUST),
        ProjectKind::Gradle => ("./gradlew test", "gradle:8-jdk21"),
        ProjectKind::SwiftPm => ("swift test", "swift:latest"),
        ProjectKind::Xcode | ProjectKind::Unknown => ("", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cargo_gradle_swiftpm_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect(dir.path()), ProjectKind::Unknown);
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert_eq!(detect(dir.path()), ProjectKind::Rust);

        let g = tempfile::tempdir().unwrap();
        std::fs::write(g.path().join("settings.gradle.kts"), "// root").unwrap();
        assert_eq!(detect(g.path()), ProjectKind::Gradle);

        let s = tempfile::tempdir().unwrap();
        std::fs::write(
            s.path().join("Package.swift"),
            "// swift-tools-version:5.9\n",
        )
        .unwrap();
        assert_eq!(detect(s.path()), ProjectKind::SwiftPm);

        let x = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(x.path().join("App.xcodeproj")).unwrap();
        assert_eq!(detect(x.path()), ProjectKind::Xcode);
    }

    #[test]
    fn defaults_are_deterministic_and_xcode_defers() {
        assert_eq!(default_lint(ProjectKind::Gradle), "./gradlew classes");
        assert_eq!(default_verify(ProjectKind::Rust).0, "cargo test --quiet");
        assert_eq!(default_verify(ProjectKind::Xcode), ("", ""));
        assert_eq!(default_lint(ProjectKind::Xcode), "");
    }
}
