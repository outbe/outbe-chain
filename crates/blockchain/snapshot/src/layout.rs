//! Check native source paths against each other, protected identities and output.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

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
/// Native sources must exist. Protected and output paths may be absent; their
/// existing ancestors are resolved so an alias cannot hide a nested path.
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
            return Err(invalid("native domain labels must be nonempty and unique"));
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
        if roots[..index].iter().any(|other| overlap(root, other)) {
            return Err(invalid("native source domains overlap"));
        }
        if protected.iter().any(|path| overlap(root, path)) {
            return Err(invalid(
                "native source overlaps protected identity or configuration",
            ));
        }
        if outputs.iter().any(|path| overlap(root, path)) {
            return Err(invalid(
                "snapshot output or scratch overlaps a native source",
            ));
        }
    }
    for (index, output) in outputs.iter().enumerate() {
        if protected.iter().any(|path| overlap(output, path)) {
            return Err(invalid(
                "snapshot output or scratch overlaps a protected path",
            ));
        }
        if outputs[..index].iter().any(|other| overlap(output, other)) {
            return Err(invalid("snapshot output and scratch paths overlap"));
        }
    }
    Ok(())
}

fn overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn resolve_existing_ancestor(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(invalid("filesystem path must not be empty"));
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
                        .ok_or_else(|| invalid("unresolvable filesystem path"))?,
                );
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| invalid("filesystem path has no existing ancestor"))?;
            }
            Err(error) => return Err(error),
        }
    }
}
