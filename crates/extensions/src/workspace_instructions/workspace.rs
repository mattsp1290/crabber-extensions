use super::failure;
use cap_std::{ambient_authority, fs::Dir};
use crabber::ExtensionError;
use std::path::{Path, PathBuf};

/// Host-created capability for a trusted boundary-to-root directory chain.
/// Handles pin admitted directories; the package never opens ambient paths
/// during prompt callbacks. Hard links and instruction contents remain trusted.
pub struct TrustedWorkspace {
    root: PathBuf,
    pub(super) chain: Vec<Dir>,
}
impl TrustedWorkspace {
    /// Resolve and admit absolute directories once, under host control.
    /// Persist the returned canonical root as Crabber's session directory.
    pub fn open(boundary: &Path, root: &Path) -> Result<Self, ExtensionError> {
        if !boundary.is_absolute() || !root.is_absolute() {
            return Err(failure("workspace"));
        }
        let boundary = boundary.canonicalize().map_err(|_| failure("workspace"))?;
        let root = root.canonicalize().map_err(|_| failure("workspace"))?;
        let relative = root
            .strip_prefix(&boundary)
            .map_err(|_| failure("boundary"))?;
        let mut chain = vec![
            Dir::open_ambient_dir(&boundary, ambient_authority())
                .map_err(|_| failure("boundary"))?,
        ];
        let mut current = PathBuf::new();
        for component in relative.components() {
            if chain.len() >= 64 {
                return Err(failure("chain-depth"));
            }
            current.push(component);
            chain.push(
                chain[0]
                    .open_dir(&current)
                    .map_err(|_| failure("workspace"))?,
            );
        }
        Ok(Self { root, chain })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}
