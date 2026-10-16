//! Check native source paths against each other, protected identities and output.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use crate::errors::LayoutError;

/// One named native source. Names are labels, never filesystem authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDomain {
    pub name: String,
    pub root: PathBuf,
}

/// Files or directories whose contents must not enter the snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProtectedPaths(pub Vec<PathBuf>);

/// Reject aliases and overlapping source/protected/output paths without writes.
///
/// Native sources must exist. Protected and output paths may be absent. This
/// function resolves their existing ancestors, so an alias cannot hide a nested path.
/// This checks a stopped layout, not concurrent filesystem replacement. The
/// archive reader/writer must separately enforce contained, no-follow access.
pub fn validate_layout(
    domains: &[ResolvedDomain],
    protected: &ProtectedPaths,
    outputs: &[PathBuf],
) -> io::Result<()> {
    let mut names = BTreeSet::new();
    let mut roots = Vec::with_capacity(domains.len());
    for domain in domains {
        if domain.name.is_empty() || !names.insert(&domain.name) {
            return Err(LayoutError::InvalidDomainLabels.into());
        }
        roots.push(fs::canonicalize(&domain.root)?);
    }
    let protected = protected
        .0
        .iter()
        .map(|path| resolve_existing_ancestor(path))
        .collect::<io::Result<Vec<_>>>()?;
    let outputs = outputs
        .iter()
        .map(|path| resolve_existing_ancestor(path))
        .collect::<io::Result<Vec<_>>>()?;

    for (index, root) in roots.iter().enumerate() {
        validate_source_separation(root, &roots[..index], &protected, &outputs)?;
    }
    for (index, output) in outputs.iter().enumerate() {
        if protected.iter().any(|path| overlap(output, path)) {
            return Err(LayoutError::OutputOverlapsProtectedPath.into());
        }
        if outputs[..index].iter().any(|other| overlap(output, other)) {
            return Err(LayoutError::OutputAndScratchOverlap.into());
        }
    }
    Ok(())
}

fn validate_source_separation(
    root: &Path,
    previous_roots: &[PathBuf],
    protected: &[PathBuf],
    outputs: &[PathBuf],
) -> io::Result<()> {
    if previous_roots.iter().any(|other| overlap(root, other)) {
        return Err(LayoutError::SourceDomainsOverlap.into());
    }
    if protected.iter().any(|path| overlap(root, path)) {
        return Err(LayoutError::SourceOverlapsProtectedPath.into());
    }
    if outputs.iter().any(|path| overlap(root, path)) {
        return Err(LayoutError::OutputOverlapsNativeSource.into());
    }
    Ok(())
}

fn overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn resolve_existing_ancestor(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(LayoutError::EmptyFilesystemPath.into());
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match fs::canonicalize(ancestor) {
            Ok(mut canonical) => {
                for component in suffix.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A dangling symlink is not an absent target we may create.
                if fs::symlink_metadata(ancestor).is_ok() {
                    return Err(error);
                }
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or(LayoutError::UnresolvableFilesystemPath)?,
                );
                ancestor = ancestor.parent().ok_or(LayoutError::NoExistingAncestor)?;
            }
            Err(error) => return Err(error),
        }
    }
}
