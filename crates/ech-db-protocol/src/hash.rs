use blake2b_simd::Params;

pub struct Blake2b256;

impl Blake2b256 {
    pub fn hash(parts: &[&[u8]]) -> [u8; 32] {
        let mut state = Params::new().hash_length(32).to_state();
        for part in parts {
            state.update(part);
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(state.finalize().as_bytes());
        out
    }
}
