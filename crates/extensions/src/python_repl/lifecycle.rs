//! Concurrent, deadline-bounded cleanup of every admitted owner.
use super::{failure, manager::Manager};
use crabber::ExtensionError;
use std::{sync::Arc, time::Duration};
use tokio::time::{Instant, timeout_at};
impl Manager {
    pub(super) async fn close(self: &Arc<Self>, bound: Duration) -> Result<(), ExtensionError> {
        let deadline = Instant::now() + bound;
        let owners: Vec<_> = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Ok(());
            }
            state.closing = true;
            state.owners.values().cloned().collect()
        };
        for owner in &owners {
            owner.lifecycle.cancel();
        }
        let mut tasks = Vec::new();
        for owner in owners {
            let manager = self.clone();
            tasks.push(self.cleanup.tracker().spawn(async move {
                timeout_at(deadline, async {
                    let mut slot = owner.gate.lock().await;
                    if slot.closed {
                        return Ok(());
                    }
                    if slot.quarantined {
                        return Err(failure("cleanup-incomplete"));
                    }
                    manager.reset(&owner, &mut slot, None).await?;
                    if let Some(dirs) = slot.dirs.as_ref() {
                        std::fs::remove_dir_all(dirs).map_err(|_| failure("cleanup-incomplete"))?;
                        if dirs
                            .try_exists()
                            .map_err(|_| failure("cleanup-incomplete"))?
                        {
                            return Err(failure("cleanup-incomplete"));
                        }
                        slot.dirs = None;
                    }
                    slot.closed = true;
                    Ok(())
                })
                .await
                .map_err(|_| failure("cleanup-incomplete"))?
            }));
        }
        let mut success = true;
        for task in tasks {
            if !matches!(timeout_at(deadline, task).await, Ok(Ok(Ok(())))) {
                success = false;
            }
        }
        if !success {
            return Err(failure("cleanup-incomplete"));
        }
        self.state.lock().unwrap().closed = true;
        Ok(())
    }
}
