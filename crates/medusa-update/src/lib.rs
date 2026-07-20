//! Secure, release-manifest-driven self-update primitives.
//!
//! The CLI owns user interaction; this crate owns version selection, release
//! discovery, integrity checks, archive extraction, and atomic installation.

mod github;
mod install;
mod model;

pub use github::{GithubReleaseClient, ReleaseClient};
pub use install::{AtomicInstaller, InstallKind, InstallLocation, Restart};
pub use model::{
    Artifact, AttestationVerifier, GithubAttestationVerifier, Platform, Release, UpdateCheck,
    UpdatePolicy, verify_sha256,
};

use std::{io::Read, path::Path};

use medusa_core::MedusaResult;

/// Streams a download while reporting cumulative bytes to a caller-owned UI.
pub fn copy_with_progress(
    reader: &mut impl Read,
    destination: &Path,
    total_bytes: Option<u64>,
    mut progress: impl FnMut(u64, Option<u64>),
) -> MedusaResult<u64> {
    use std::{fs::File, io::Write};

    let mut output = File::create(destination)?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut copied = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            output.flush()?;
            return Ok(copied);
        }
        output.write_all(&buffer[..count])?;
        copied += count as u64;
        progress(copied, total_bytes);
    }
}
