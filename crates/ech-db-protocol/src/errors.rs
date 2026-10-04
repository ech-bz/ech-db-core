use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("bcs: {0}")]
    Bcs(#[from] bcs::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error("author does not match signing key")]
    AuthorMismatch,
    #[error(transparent)]
    Codec(#[from] CodecError),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum RetryClass {
    Never,
    Retry,
    UnknownOutcome,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct DbError {
    pub class: RetryClass,
    pub code: DbErrorCode,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum DbErrorCode {
    Unauthorized,
    BadBcs,
    Protocol,
    Abi,
    UnsupportedProfile { profile: u32 },
    InvalidWasm,
    ProgramNotFound,
    SequenceMismatch { current: u64, expected: u64 },
    Application { bytes: Vec<u8> },
    VmTrap,
    OutOfFuel,
    ResourceLimit,
    Unavailable,
    RecoveryRequired,
    Integrity,
    Fdb { code: i32 },
}

impl DbError {
    pub fn never(code: DbErrorCode) -> Self {
        Self {
            class: RetryClass::Never,
            code,
        }
    }

    pub fn unauthorized() -> Self {
        Self::never(DbErrorCode::Unauthorized)
    }

    pub fn bad_bcs() -> Self {
        Self::never(DbErrorCode::BadBcs)
    }

    pub fn protocol() -> Self {
        Self::never(DbErrorCode::Protocol)
    }

    pub fn abi() -> Self {
        Self::never(DbErrorCode::Abi)
    }

    pub fn invalid_wasm() -> Self {
        Self::never(DbErrorCode::InvalidWasm)
    }

    pub fn unsupported_profile(profile: u32) -> Self {
        Self::never(DbErrorCode::UnsupportedProfile { profile })
    }

    pub fn program_not_found() -> Self {
        Self::never(DbErrorCode::ProgramNotFound)
    }

    pub fn sequence_mismatch(current: u64, expected: u64) -> Self {
        Self::never(DbErrorCode::SequenceMismatch { current, expected })
    }

    pub fn application(bytes: Vec<u8>) -> Self {
        Self::never(DbErrorCode::Application { bytes })
    }

    pub fn vm_trap() -> Self {
        Self::never(DbErrorCode::VmTrap)
    }

    pub fn out_of_fuel() -> Self {
        Self::never(DbErrorCode::OutOfFuel)
    }

    pub fn resource_limit() -> Self {
        Self::never(DbErrorCode::ResourceLimit)
    }

    pub fn unavailable() -> Self {
        Self {
            class: RetryClass::Retry,
            code: DbErrorCode::Unavailable,
        }
    }

    pub fn recovery_required() -> Self {
        Self::never(DbErrorCode::RecoveryRequired)
    }

    pub fn integrity() -> Self {
        Self::never(DbErrorCode::Integrity)
    }

    pub fn fdb(code: i32, class: RetryClass) -> Self {
        Self {
            class,
            code: DbErrorCode::Fdb { code },
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self.class, RetryClass::Retry | RetryClass::UnknownOutcome)
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?}/{:?}", self.class, self.code)
    }
}

impl std::error::Error for DbError {}
