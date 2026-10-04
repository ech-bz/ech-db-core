use std::sync::Arc;

use futures::StreamExt;

use crate::config::Config;
use crate::exec::Executor;
use crate::exporter::ExportOutcome;
use crate::fdb::Fdb;
use crate::root::Roots;
use crate::s3::S3;
use crate::sui::Sui;
use crate::wasm::Profile;

pub struct Server {
    pub config: Config,
    pub fdb: Fdb,
    pub executor: Executor,
    pub roots: Roots,
    pub s3: S3,
    pub sui: Sui,
}

impl Server {
    pub fn new(config: Config, fdb: Fdb) -> Result<Arc<Self>, crate::errors::Error> {
        let sui = Sui::new(&config.sui)?;
        let s3 = S3::new(&config.s3);
        Ok(Arc::new(Self {
            config,
            fdb,
            executor: Executor::new(Profile::engine()),
            roots: Roots::new(),
            s3,
            sui,
        }))
    }

    pub async fn run(self: Arc<Self>) -> Result<(), crate::errors::Error> {
        let listener = tokio::net::TcpListener::bind(self.config.listen).await?;
        self.run_with_listener(listener).await
    }

    pub async fn run_with_listener(
        self: Arc<Self>,
        listener: tokio::net::TcpListener,
    ) -> Result<(), crate::errors::Error> {
        let service = crate::session::ExchangeServer::new(self.clone());
        let background = self.clone();
        tokio::spawn(async move {
            background.background_loop().await;
        });
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener).map(|socket| {
            if let Ok(stream) = &socket {
                let _ = stream.set_nodelay(true);
            }
            socket
        });
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(incoming)
            .await?;
        Ok(())
    }

    async fn background_loop(self: Arc<Self>) {
        let mut stopped = std::collections::HashSet::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            for root in self.roots.snapshot() {
                if stopped.contains(&root) {
                    continue;
                }
                if let Err(error) = self.close_windows(root).await {
                    tracing::warn!(error = %error, "window close failed");
                }
                loop {
                    match self.export_once(root).await {
                        Ok(ExportOutcome::Exported) => continue,
                        Ok(ExportOutcome::Idle) => break,
                        Err(error) => {
                            tracing::warn!(error = %error, "export failed");
                            if !error.is_retryable() {
                                stopped.insert(root);
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
}
