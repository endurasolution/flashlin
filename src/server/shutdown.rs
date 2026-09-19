use tokio::sync::broadcast;

/// A one-shot shutdown signal.  Created by `ShutdownTrigger::subscribe()`.
pub struct ShutdownSignal {
    receiver: broadcast::Receiver<()>,
}

impl ShutdownSignal {
    pub async fn wait(&mut self) {
        // Ignore RecvError — it means the sender was dropped, which is also a shutdown.
        let _ = self.receiver.recv().await;
    }
}

pub struct ShutdownTrigger {
    sender: broadcast::Sender<()>,
}

impl ShutdownTrigger {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        ShutdownTrigger { sender: tx }
    }

    /// Create a new receiver for this shutdown signal.
    pub fn subscribe(&self) -> ShutdownSignal {
        ShutdownSignal {
            receiver: self.sender.subscribe(),
        }
    }

    pub fn fire(&self) {
        let _ = self.sender.send(());
    }
}

/// Wait for Ctrl-C and return.
pub async fn wait_for_ctrl_c() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install Ctrl-C handler");
}
