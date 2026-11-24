use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use thiserror::Error;
use zeroize::Zeroizing;

pub type SeedBytes = [u8; 32];

#[derive(Clone)]
pub struct Seed(Zeroizing<SeedBytes>);

impl fmt::Debug for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Seed([REDACTED])")
    }
}

#[derive(Debug, Error)]
pub enum SeedError {
    #[error("seed environment variable is not set")]
    EnvMissing,
    #[error("seed command failed to start")]
    CommandStart,
    #[error("seed command failed")]
    CommandFailed,
    #[error("seed must be 32 raw bytes or 64 hex characters")]
    Length,
    #[error("seed hex contains a non-hex character")]
    Hex,
    #[error("seed file must not be a symlink")]
    Symlink,
    #[error("seed file must not be readable by group or others")]
    Permissions,
    #[error("seed file is too large")]
    TooLarge,
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl Seed {
    pub fn from_bytes(bytes: SeedBytes) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn as_bytes(&self) -> &SeedBytes {
        &self.0
    }

    pub fn parse(bytes: &[u8], trim: bool) -> Result<Self, SeedError> {
        let bytes = if trim { bytes.trim_ascii() } else { bytes };
        match bytes.len() {
            32 => {
                let mut seed = [0u8; 32];
                seed.copy_from_slice(bytes);
                Ok(Self::from_bytes(seed))
            }
            64 => {
                let mut seed = [0u8; 32];
                for (slot, pair) in seed.iter_mut().zip(bytes.chunks_exact(2)) {
                    *slot = hex_digit(pair[0])? << 4 | hex_digit(pair[1])?;
                }
                Ok(Self::from_bytes(seed))
            }
            _ => Err(SeedError::Length),
        }
    }

    pub fn from_file(path: &Path) -> Result<Self, SeedError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::symlink_metadata(path)?;
            if meta.file_type().is_symlink() {
                return Err(SeedError::Symlink);
            }
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(SeedError::Permissions);
            }
            if meta.len() > 64 {
                return Err(SeedError::TooLarge);
            }
        }
        Self::parse(&fs::read(path)?, false)
    }

    pub fn from_env(name: &str) -> Result<Self, SeedError> {
        let value = std::env::var(name).map_err(|_| SeedError::EnvMissing)?;
        Self::parse(value.as_bytes(), true)
    }

    pub fn from_command(command: &str) -> Result<Self, SeedError> {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .map_err(|_| SeedError::CommandStart)?;
        if !output.status.success() {
            return Err(SeedError::CommandFailed);
        }
        Self::parse(&output.stdout, true)
    }
}

fn hex_digit(byte: u8) -> Result<u8, SeedError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(SeedError::Hex),
    }
}
