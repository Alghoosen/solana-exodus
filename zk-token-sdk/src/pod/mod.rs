pub mod convert;
// pub mod decryption;
pub mod elgamal;
// pub mod ops;
pub mod auth_encryption;
pub mod pedersen;
pub mod pod;

use {
    crate::zk_token_proof_instruction::ProofType,
    num_traits::{FromPrimitive, ToPrimitive},
    solana_program::instruction::InstructionError,
};

pub use {
    bytemuck::{Pod, Zeroable},
    elgamal::{DecryptHandle, ElGamalCiphertext, ElGamalPubkey},
    pedersen::PedersenCommitment,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(transparent)]
pub struct PodU16([u8; 2]);
impl From<u16> for PodU16 {
    fn from(n: u16) -> Self {
        Self(n.to_le_bytes())
    }
}
impl From<PodU16> for u16 {
    fn from(pod: PodU16) -> Self {
        Self::from_le_bytes(pod.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(transparent)]
pub struct PodU64([u8; 8]);
impl From<u64> for PodU64 {
    fn from(n: u64) -> Self {
        Self(n.to_le_bytes())
    }
}
impl From<PodU64> for u64 {
    fn from(pod: PodU64) -> Self {
        Self::from_le_bytes(pod.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(transparent)]
pub struct PodProofType(u8);
impl From<ProofType> for PodProofType {
    fn from(proof_type: ProofType) -> Self {
        Self(ToPrimitive::to_u8(&proof_type).unwrap())
    }
}
impl TryFrom<PodProofType> for ProofType {
    type Error = InstructionError;

    fn try_from(pod: PodProofType) -> Result<Self, Self::Error> {
        FromPrimitive::from_u8(pod.0).ok_or(Self::Error::InvalidAccountData)
    }
}
