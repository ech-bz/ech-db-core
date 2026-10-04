use std::collections::HashSet;
use std::sync::Mutex;

use ech_db_protocol::errors::DbError;
use ech_db_protocol::keys::SysKey;
use ech_db_protocol::values::Head;

use crate::server::Server;
use crate::windows::Clock;

pub struct Roots {
    open: Mutex<HashSet<[u8; 32]>>,
}

impl Roots {
    pub fn new() -> Self {
        Self {
            open: Mutex::new(HashSet::new()),
        }
    }

    pub fn is_open(&self, root: &[u8; 32]) -> bool {
        let open = self.open.lock().unwrap_or_else(|poison| poison.into_inner());
        open.contains(root)
    }

    pub fn register(&self, root: [u8; 32]) {
        let mut open = self.open.lock().unwrap_or_else(|poison| poison.into_inner());
        open.insert(root);
    }

    pub fn snapshot(&self) -> Vec<[u8; 32]> {
        let open = self.open.lock().unwrap_or_else(|poison| poison.into_inner());
        open.iter().copied().collect()
    }
}

impl Default for Roots {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    pub async fn ensure_root(&self, root: [u8; 32]) -> Result<(), DbError> {
        if self.roots.is_open(&root) {
            return Ok(());
        }
        let trx = self.fdb.trx()?;
        if trx.get_raw(&SysKey::head(&root)).await?.is_some() {
            self.roots.register(root);
            return Ok(());
        }
        let anchor = self
            .sui
            .anchor_of(&root)
            .await
            .map_err(|error| crate::sui::SuiErrors::db(&error))?;
        if anchor.is_some() {
            return Err(DbError::recovery_required());
        }
        let head = Head::initial(Clock::now_unix_ms(), self.config.window.length_ms())
            .ok_or_else(DbError::integrity)?;
        trx.set_bcs(&SysKey::head(&root), &head)?;
        trx.set_raw(&SysKey::window_number(&root, 0), b"");
        trx.commit().await?;
        self.roots.register(root);
        Ok(())
    }
}
