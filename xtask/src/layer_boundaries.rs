use std::{
    fmt, fs,
    path::{Path, PathBuf},
    process::Command,
};

mod deps;
mod lexer;
mod lexer_strings;
mod scanner;

#[cfg(test)]
use deps::{find_resolved_tree_violations, inspect_metadata_edges, scan_blockchain_sources};
#[cfg(test)]
mod tests;

const BLOCKCHAIN_REL_DIR: &str = "crates/blockchain";

const READ_ONLY_RELEASE_ALLOWLIST: [&str; 2] = [
    "release/project-toolchain-v1.json",
    "release/dcap-native-qvl-v1.json",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundaryReport {
    pub blockchain_packages: usize,
    pub scanned_source_files: usize,
    pub verified_inclusions: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DependencyEdgeKind {
    Normal,
    Build,
    Dev,
    Unknown,
}

impl DependencyEdgeKind {
    pub(super) fn from_cargo(kind: Option<&str>) -> Self {
        match kind {
            None | Some("normal") => Self::Normal,
            Some("build") => Self::Build,
            Some("dev") => Self::Dev,
            Some(_) => Self::Unknown,
        }
    }
}

impl fmt::Display for DependencyEdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Normal => "normal",
            Self::Build => "build",
            Self::Dev => "dev",
            Self::Unknown => "unknown",
        };
        f.write_str(label)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InclusionKind {
    ModulePath,
    IncludeMacro,
    IncludeStr,
    IncludeBytes,
}

impl InclusionKind {
    pub(super) const fn allows_release_pin_allowlist(self) -> bool {
        matches!(self, Self::IncludeStr | Self::IncludeBytes)
    }
}

impl fmt::Display for InclusionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::ModulePath => "#[path]",
            Self::IncludeMacro => "include!",
            Self::IncludeStr => "include_str!",
            Self::IncludeBytes => "include_bytes!",
        };
        f.write_str(label)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayerViolation {
    ForbiddenManifestDependency {
        source_package: String,
        kind: DependencyEdgeKind,
        target_package: String,
        target_manifest: PathBuf,
    },
    ForbiddenResolvedTreeEdge {
        target_package: String,
    },
    ForbiddenFileInclusion {
        source_file: PathBuf,
        line: usize,
        directive: InclusionKind,
        raw_target: String,
        resolved_path: PathBuf,
    },
}

impl fmt::Display for LayerViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForbiddenManifestDependency {
                source_package,
                kind,
                target_package,
                target_manifest,
            } => write!(
                f,
                "{source_package} has forbidden {kind} dependency on workspace package {target_package} ({})",
                target_manifest.display()
            ),
            Self::ForbiddenResolvedTreeEdge { target_package } => write!(
                f,
                "crates/blockchain resolved production tree reaches outside workspace package {target_package}"
            ),
            Self::ForbiddenFileInclusion {
                source_file,
                line,
                directive,
                raw_target,
                resolved_path,
            } => write!(
                f,
                "{}:{line}: {directive} target {raw_target:?} resolves outside crates/blockchain ({})",
                source_file.display(),
                resolved_path.display()
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LayerBoundaryError {
    #[error("failed to canonicalize {path}: {source}", path = path.display())]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to walk {path}: {source}", path = path.display())]
    Walk {
        path: PathBuf,
        #[source]
        source: walkdir::Error,
    },
    #[error("failed to read {path}: {source}", path = path.display())]
    ReadSource {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to spawn `{program}`: {source}")]
    CommandSpawn {
        program: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("command `{command}` failed: {stderr}")]
    CommandFailed { command: String, stderr: String },
    #[error("failed to parse `cargo metadata` JSON: {0}")]
    MetadataParse(#[from] serde_json::Error),
    #[error("{}:{line}: unsupported non-literal {directive} expression", source_file.display())]
    UnparsableInclusion {
        source_file: PathBuf,
        line: usize,
        directive: InclusionKind,
    },
    #[error(
        "{}:{line}: {directive} target {} could not be resolved: {source}",
        source_file.display(),
        target.display()
    )]
    UnresolvedTarget {
        source_file: PathBuf,
        line: usize,
        directive: InclusionKind,
        target: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("crates/blockchain layer boundary violations ({}):\n{}", violations.len(), format_violations(violations))]
    Violations { violations: Vec<LayerViolation> },
}

fn format_violations(violations: &[LayerViolation]) -> String {
    violations
        .iter()
        .map(|violation| format!("  - {violation}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn check_layer_boundaries(repo_root: &Path) -> Result<BoundaryReport, LayerBoundaryError> {
    let canonical_root = canonicalize_path(repo_root)?;
    let metadata_json = run_cargo(
        &canonical_root,
        &["metadata", "--locked", "--format-version", "1", "--no-deps"],
    )?;
    let (layout, mut violations) = deps::inspect_metadata_edges(&canonical_root, &metadata_json)?;

    if !layout.blockchain_packages.is_empty() {
        let mut args = vec![
            "tree",
            "--locked",
            "-e",
            "features,no-dev",
            "--prefix",
            "none",
        ];
        let pkg_names: Vec<&str> = layout
            .blockchain_packages
            .iter()
            .map(String::as_str)
            .collect();
        for pkg in &pkg_names {
            args.push("-p");
            args.push(pkg);
        }
        let tree_output = run_cargo(&canonical_root, &args)?;
        violations.extend(deps::find_resolved_tree_violations(
            &tree_output,
            &layout.outside_packages,
        ));
    }

    let (scanned_source_files, verified_inclusions, inclusion_violations) =
        deps::scan_blockchain_sources(&layout.repo_root, &layout.blockchain_root)?;
    violations.extend(inclusion_violations);

    if !violations.is_empty() {
        return Err(LayerBoundaryError::Violations { violations });
    }

    Ok(BoundaryReport {
        blockchain_packages: layout.blockchain_packages.len(),
        scanned_source_files,
        verified_inclusions,
    })
}

pub(super) fn canonicalize_path(path: &Path) -> Result<PathBuf, LayerBoundaryError> {
    fs::canonicalize(path).map_err(|source| LayerBoundaryError::Canonicalize {
        path: path.to_path_buf(),
        source,
    })
}

fn run_cargo(repo_root: &Path, args: &[&str]) -> Result<String, LayerBoundaryError> {
    let output = Command::new("cargo")
        .args(args)
        .current_dir(repo_root)
        .output()
        .map_err(|source| LayerBoundaryError::CommandSpawn {
            program: "cargo",
            source,
        })?;
    if !output.status.success() {
        return Err(LayerBoundaryError::CommandFailed {
            command: format!("cargo {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
