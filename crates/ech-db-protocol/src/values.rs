use serde::{Deserialize, Serialize};

use crate::errors::{CodecError, SignError};
use crate::hash::Blake2b256;

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ProgramBundle {
    pub format_version: u32,
    pub execution_profile: u32,
    pub wasm: Vec<u8>,
}

impl ProgramBundle {
    pub const FORMAT_VERSION: u32 = 1;

    pub fn hash(&self) -> Result<[u8; 32], CodecError> {
        let encoded = bcs::to_bytes(self)?;
        Ok(Blake2b256::hash(&[encoded.as_slice()]))
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Invocation {
    pub program_hash: [u8; 32],
    pub function: String,
    pub args: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct IntentBody {
    pub version: u32,
    pub root: [u8; 32],
    pub author: [u8; 32],
    pub tweak: [u8; 32],
    pub expected_seq: u64,
    pub call: Invocation,
}

impl IntentBody {
    pub const FORMAT_VERSION: u32 = 1;

    const PREFIX: &'static [u8] = b"intent";

    pub fn signing_bytes(&self) -> Result<Vec<u8>, CodecError> {
        let mut bytes = Self::PREFIX.to_vec();
        bytes.extend_from_slice(&bcs::to_bytes(self)?);
        Ok(bytes)
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct SignedIntent {
    pub body: IntentBody,
    #[serde(with = "crate::fixed64")]
    pub signature: [u8; 64],
}

impl SignedIntent {
    pub fn sign(body: IntentBody, key: &ed25519_dalek::SigningKey) -> Result<Self, SignError> {
        use ed25519_dalek::Signer;
        if body.author != key.verifying_key().to_bytes() {
            return Err(SignError::AuthorMismatch);
        }
        let signature = key.sign(&body.signing_bytes()?).to_bytes();
        Ok(Self { body, signature })
    }

    pub fn verify(&self) -> bool {
        let Ok(bytes) = self.body.signing_bytes() else {
            return false;
        };
        let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(&self.body.author) else {
            return false;
        };
        let signature = ed25519_dalek::Signature::from_bytes(&self.signature);
        key.verify_strict(&bytes, &signature).is_ok()
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct AttestationBody {
    pub responses: Vec<u8>,
    pub timestamp_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Attestation {
    pub body: AttestationBody,
    #[serde(with = "crate::fixed64")]
    pub signature: [u8; 64],
}

impl Attestation {
    const PREFIX: &'static [u8] = b"attest";

    pub fn signing_bytes(
        author_signature: &[u8; 64],
        body: &AttestationBody,
    ) -> Result<Vec<u8>, CodecError> {
        let mut bytes = Self::PREFIX.to_vec();
        bytes.extend_from_slice(author_signature);
        bytes.extend_from_slice(&bcs::to_bytes(body)?);
        Ok(bytes)
    }

    pub fn sign(
        author_signature: &[u8; 64],
        body: AttestationBody,
        key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, SignError> {
        use ed25519_dalek::Signer;
        let bytes = Self::signing_bytes(author_signature, &body)?;
        Ok(Self {
            body,
            signature: key.sign(&bytes).to_bytes(),
        })
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct AttestedIntent {
    pub intent: SignedIntent,
    pub attestation: Attestation,
}

impl AttestedIntent {
    pub fn attest(
        intent: &SignedIntent,
        key: &ed25519_dalek::SigningKey,
        responses: Vec<u8>,
        timestamp_ms: u64,
    ) -> Result<Self, SignError> {
        let attestation = Attestation::sign(
            &intent.signature,
            AttestationBody {
                responses,
                timestamp_ms,
            },
            key,
        )?;
        Ok(Self {
            intent: intent.clone(),
            attestation,
        })
    }

    pub fn verify_attestation(&self) -> bool {
        let Ok(bytes) = Attestation::signing_bytes(&self.intent.signature, &self.attestation.body)
        else {
            return false;
        };
        let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(&self.intent.body.root) else {
            return false;
        };
        let signature = ed25519_dalek::Signature::from_bytes(&self.attestation.signature);
        key.verify_strict(&bytes, &signature).is_ok()
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct IntentRef {
    pub author: [u8; 32],
    pub seq: u64,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum ArchiveRecord {
    ProgramChunk {
        program_hash: [u8; 32],
        chunk_seq: u64,
        chunk_count: u64,
        bytes: Vec<u8>,
    },
    Intent(AttestedIntent),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct WindowBlob {
    pub format_version: u32,
    pub root: [u8; 32],
    pub window_number: u64,
    pub prev_hash: [u8; 32],
    pub records: Vec<ArchiveRecord>,
}

impl WindowBlob {
    pub const FORMAT_VERSION: u32 = 1;

    pub fn hash(&self) -> Result<[u8; 32], CodecError> {
        let encoded = bcs::to_bytes(self)?;
        Ok(Blake2b256::hash(&[encoded.as_slice()]))
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Head {
    pub number: u64,
    pub closes_at_unix_ms: u64,
}

impl Head {
    pub fn initial(now_unix_ms: u64, window_ms: u64) -> Option<Self> {
        let closes_at_unix_ms = now_unix_ms
            .checked_div(window_ms)?
            .checked_add(1)?
            .checked_mul(window_ms)?;
        Some(Self {
            number: 0,
            closes_at_unix_ms,
        })
    }

    pub fn is_due(&self, now_unix_ms: u64) -> bool {
        now_unix_ms >= self.closes_at_unix_ms
    }

    pub fn next(&self, window_ms: u64) -> Option<Self> {
        Some(Self {
            number: self.number.checked_add(1)?,
            closes_at_unix_ms: self.closes_at_unix_ms.checked_add(window_ms)?,
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExportCursor {
    pub number: u64,
    pub hash: [u8; 32],
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum CallOutput {
    Ok(Vec<u8>),
    Err(Vec<u8>),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct RecoveryProgress {
    pub window: u64,
    pub processed: u64,
}

pub struct ProgramChunks;

impl ProgramChunks {
    pub const CHUNK_SIZE: usize = 50 * 1024;

    pub fn split(bytes: &[u8]) -> Vec<&[u8]> {
        bytes.chunks(Self::CHUNK_SIZE).collect()
    }
}
