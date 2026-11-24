use std::fmt;
use std::io;
use std::path::Path;

use thiserror::Error;
use zeroize::Zeroizing;

const MAX_SEED_FILE_BYTES: u64 = 128;
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
        let file = crate::fileio::open_no_follow(path).map_err(|error| {
            if crate::fileio::is_symlink_error(&error) {
                SeedError::Symlink
            } else {
                SeedError::Io(error)
            }
        })?;
        let metadata = file.metadata()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(SeedError::Permissions);
            }
        }
        if metadata.len() > MAX_SEED_FILE_BYTES {
            return Err(SeedError::TooLarge);
        }
        let bytes = Zeroizing::new(
            crate::fileio::read_limited(&file, MAX_SEED_FILE_BYTES).map_err(|error| {
                if error.kind() == io::ErrorKind::InvalidData {
                    SeedError::TooLarge
                } else {
                    SeedError::Io(error)
                }
            })?,
        );
        match Self::parse(bytes.as_slice(), false) {
            Ok(seed) => Ok(seed),
            Err(SeedError::Length) => Self::parse(bytes.as_slice().trim_ascii(), false),
            Err(error) => Err(error),
        }
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
