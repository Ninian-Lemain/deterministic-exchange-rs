//! Cold-path checkpoint bundles. Storage is never called by command admission.
use crate::{BuildError, ConfigError, EngineConfig};
use hft_recovery::Snapshot;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

#[derive(Debug)]
pub enum BundleError {
    Io(io::Error),
    Incomplete,
    IntegrityMismatch,
    Configuration(ConfigError),
    Recovery(BuildError),
}

/// Explicitly selected snapshot, configuration and contiguous tail.
/// A bundle is authoritative only after its completion marker is published.
#[derive(Debug)]
pub struct RecoveryBundle {
    pub configuration: EngineConfig,
    pub snapshot: Vec<u8>,
    pub tail: Vec<u8>,
}

impl RecoveryBundle {
    #[must_use]
    pub fn checkpoint(configuration: &EngineConfig, snapshot: &Snapshot) -> Self {
        Self {
            configuration: configuration.clone(),
            snapshot: snapshot.bytes().to_vec(),
            tail: Vec::new(),
        }
    }

    /// # Errors
    /// Rejects an existing destination or failed file/directory synchronization.
    /// Failures may leave an incomplete directory; readers refuse it.
    pub fn publish_new(&self, destination: &Path) -> Result<(), BundleError> {
        self.configuration
            .validate()
            .map_err(BundleError::Configuration)?;
        fs::create_dir(destination).map_err(BundleError::Io)?;
        write_new(&destination.join("config.v1"), &self.configuration.encode())?;
        write_new(&destination.join("snapshot.v1"), &self.snapshot)?;
        write_new(&destination.join("tail.v1"), &self.tail)?;
        sync_directory(destination)?;
        write_new(&destination.join("COMMITTED"), &self.marker())?;
        sync_directory(destination)?;
        if let Some(parent) = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            sync_directory(parent)?;
        }
        Ok(())
    }

    /// # Errors
    /// Rejects missing completion markers, malformed configuration and read errors.
    pub fn read(directory: &Path) -> Result<Self, BundleError> {
        let marker = fs::read(directory.join("COMMITTED")).map_err(BundleError::Io)?;
        if !marker.starts_with(b"HFTBUNDLE 1\n") {
            return Err(BundleError::Incomplete);
        }
        let configuration =
            EngineConfig::decode(&fs::read(directory.join("config.v1")).map_err(BundleError::Io)?)
                .map_err(BundleError::Configuration)?;
        let bundle = Self {
            configuration,
            snapshot: fs::read(directory.join("snapshot.v1")).map_err(BundleError::Io)?,
            tail: fs::read(directory.join("tail.v1")).map_err(BundleError::Io)?,
        };
        if marker != bundle.marker() {
            return Err(BundleError::IntegrityMismatch);
        }
        Ok(bundle)
    }

    fn marker(&self) -> Vec<u8> {
        let mut hash = Sha256::new();
        hash.update(b"deterministic-exchange bundle v1\0");
        for bytes in [&self.configuration.encode(), &self.snapshot, &self.tail] {
            hash.update((bytes.len() as u128).to_be_bytes());
            hash.update(bytes);
        }
        let mut marker = b"HFTBUNDLE 1\n".to_vec();
        marker.extend_from_slice(&hash.finalize());
        marker
    }

    /// Validate and recover through the configured engine boundary before admission opens.
    /// # Errors
    /// Rejects unsupported shapes, corrupt state, configuration mismatch or bad tail.
    pub fn restore<
        const A: usize,
        const R: usize,
        const L: usize,
        const O: usize,
        const T: usize,
    >(
        &self,
        expected: &EngineConfig,
    ) -> Result<crate::EngineBuilder<A, R, L, O, T>, BundleError> {
        crate::EngineBuilder::restore_configured(
            expected,
            &self.configuration.encode(),
            &self.snapshot,
            &self.tail,
        )
        .map_err(BundleError::Recovery)
    }

    /// Verify with the running binary's capacity shape, then publish a backup at a new path.
    /// # Errors
    /// Rejects invalid recovery input or any publication failure.
    pub fn backup_new<
        const A: usize,
        const R: usize,
        const L: usize,
        const O: usize,
        const T: usize,
    >(
        &self,
        destination: &Path,
    ) -> Result<(), BundleError> {
        self.restore::<A, R, L, O, T>(&self.configuration)?;
        self.publish_new(destination)
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), BundleError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(BundleError::Io)?;
    file.write_all(bytes).map_err(BundleError::Io)?;
    file.sync_all().map_err(BundleError::Io)
}

#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))] // Unix directory sync can fail.
fn sync_directory(path: &Path) -> Result<(), BundleError> {
    #[cfg(unix)]
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(BundleError::Io)?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
