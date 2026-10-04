use bcs::to_bytes;
use serde::Serialize;

use ech_db_protocol::errors::CodecError;
use ech_db_protocol::values::Invocation;

use crate::error::Error;

pub struct Call {
    program_hash: [u8; 32],
    function: String,
    args: Vec<u8>,
}

impl Call {
    pub fn new(program_hash: [u8; 32], function: &str) -> Self {
        Self {
            program_hash,
            function: function.to_string(),
            args: Vec::new(),
        }
    }

    pub fn with_args<T: Serialize>(mut self, args: &T) -> Result<Self, Error> {
        self.args = to_bytes(args).map_err(CodecError::from)?;
        Ok(self)
    }

    pub fn invocation(&self) -> Invocation {
        Invocation {
            program_hash: self.program_hash,
            function: self.function.clone(),
            args: self.args.clone(),
        }
    }
}
