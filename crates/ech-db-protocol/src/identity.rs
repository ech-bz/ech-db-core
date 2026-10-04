use crate::hash::Blake2b256;

pub struct Identity;

impl Identity {
    pub fn seed(master: &[u8; 32], tweak: &[u8; 32]) -> [u8; 32] {
        Blake2b256::hash(&[master.as_slice(), tweak.as_slice()])
    }

    pub fn address(public_key: &[u8; 32]) -> [u8; 32] {
        Blake2b256::hash(&[&[0x00u8], public_key.as_slice()])
    }
}
