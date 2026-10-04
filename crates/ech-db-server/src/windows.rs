use ech_db_protocol::errors::DbError;
use ech_db_protocol::keys::SysKey;
use ech_db_protocol::values::Head;

use crate::server::Server;

pub struct Clock;

impl Clock {
    pub fn now_unix_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is before the unix epoch")
            .as_millis() as u64
    }
}

impl Server {
    pub async fn close_windows(&self, root: [u8; 32]) -> Result<(), DbError> {
        loop {
            let trx = self.fdb.trx()?;
            let head: Head = trx
                .get_bcs(&SysKey::head(&root))
                .await?
                .ok_or_else(DbError::integrity)?;
            if !head.is_due(Clock::now_unix_ms()) {
                return Ok(());
            }
            let next = head
                .next(self.config.window.length_ms())
                .ok_or_else(DbError::integrity)?;
            trx.set_bcs(&SysKey::head(&root), &next)?;
            trx.set_raw(&SysKey::window_number(&root, next.number), b"");
            trx.commit().await?;
        }
    }
}
