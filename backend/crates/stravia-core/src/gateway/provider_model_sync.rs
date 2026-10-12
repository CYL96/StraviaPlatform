use super::*;
use crate::provider_models::{
    ModelChangeSource, ProviderModelSyncFailure, ProviderModelSyncSummary,
};
use tokio::sync::watch;

type SharedResult = Result<ProviderModelSyncSummary, Arc<anyhow::Error>>;
#[derive(Default)]
pub(crate) struct ProviderModelSyncRuntime {
    flights: Mutex<HashMap<String, watch::Receiver<Option<SharedResult>>>>,
}
impl ProviderModelSyncRuntime {
    pub(crate) fn in_progress(&self, id: &str) -> bool {
        self.flights.lock().contains_key(id)
    }
}

struct SyncFlight {
    runtime: Arc<ProviderModelSyncRuntime>,
    provider_id: String,
}

impl Drop for SyncFlight {
    fn drop(&mut self) {
        self.runtime.flights.lock().remove(&self.provider_id);
    }
}

#[derive(Debug)]
struct SharedSyncError(Arc<anyhow::Error>);
impl std::fmt::Display for SharedSyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}
impl std::error::Error for SharedSyncError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}

impl Gateway {
    pub(crate) async fn sync_provider_inventory(
        &self,
        provider_id: &str,
        source: ModelChangeSource,
    ) -> anyhow::Result<ProviderModelSyncSummary> {
        anyhow::ensure!(
            !self.lifecycle.cancellation.is_cancelled(),
            "Gateway is shutting down"
        );
        let mut receiver = {
            let mut flights = self.provider_model_sync.flights.lock();
            if let Some(receiver) = flights.get(provider_id) {
                receiver.clone()
            } else {
                let (sender, receiver) = watch::channel(None);
                flights.insert(provider_id.to_owned(), receiver.clone());
                let gw = self.background_clone();
                let flight = SyncFlight {
                    runtime: Arc::clone(&self.provider_model_sync),
                    provider_id: provider_id.to_owned(),
                };
                self.lifecycle.spawn(async move {
                    let result = crate::admin::routes::RouteModule::new(&gw)
                        .sync(&flight.provider_id, source)
                        .await
                        .map_err(Arc::new);
                    sender.send_replace(Some(result));
                    drop(flight);
                });
                receiver
            }
        };
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result.map_err(|error| {
                    let failure = error.downcast_ref::<ProviderModelSyncFailure>().cloned();
                    let shared = anyhow::Error::new(SharedSyncError(error));
                    match failure {
                        Some(failure) => shared.context(failure),
                        None => shared,
                    }
                });
            }
            receiver
                .changed()
                .await
                .map_err(|_| anyhow::anyhow!("Model synchronization interrupted by shutdown"))?;
        }
    }

    pub(crate) fn provider_model_pause_reason(
        &self,
        provider: &db::models::Provider,
        automatic: bool,
    ) -> Option<String> {
        let reason = if !automatic {
            Some("Automatic synchronization is disabled")
        } else if !provider.is_enabled {
            Some("Provider is disabled")
        } else if provider.credential_invalid() {
            Some("Provider credentials are invalid")
        } else {
            let descriptor = provider
                .vendor
                .as_deref()
                .and_then(|vendor| self.vendor_plugins.descriptor(vendor).ok());
            match descriptor {
                None => Some("Provider plugin is unavailable"),
                Some(descriptor) => {
                    let channel = provider
                        .channel
                        .as_deref()
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .unwrap_or("default");
                    if descriptor.channels.iter().any(|entry| {
                        entry.id == channel
                            && entry
                                .capabilities
                                .contains(&stravia_vendor_sdk::Capability::ModelDiscovery)
                    }) {
                        None
                    } else {
                        Some("Provider channel does not support model discovery")
                    }
                }
            }
        };
        reason.map(str::to_owned)
    }

    pub(super) async fn start_provider_model_sync(&self) -> anyhow::Result<()> {
        let startup = self.storage.providers().list().await?;
        let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let gw = self.background_clone();
        self.lifecycle.spawn(async move {
            let mut startup = Some(startup);
            loop {
                tokio::select! {
                    _ = gw.lifecycle.cancellation.cancelled() => return,
                    _ = interval.tick() => {}
                }
                let cutoff = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                if let Err(error) = gw.storage.provider_models().prune_changes(&cutoff).await {
                    tracing::warn!(error = ?error, "model change retention cleanup failed");
                }
                let providers = match startup.take() {
                    Some(providers) => providers,
                    None => match gw.storage.providers().list().await {
                    Ok(providers) => providers,
                    Err(error) => { tracing::warn!(error = ?error, "scheduled provider inventory read failed"); continue; }
                    }
                };
                for provider in providers {
                    if gw.lifecycle.cancellation.is_cancelled() { return; }
                    let state = match gw.storage.provider_models().sync_state(&provider.id).await {
                        Ok(state) => state,
                        Err(error) => { tracing::warn!(provider_id = %provider.id, error = ?error, "scheduled sync preference read failed"); continue; }
                    };
                    if gw.provider_model_pause_reason(&provider, state.automatic_sync).is_some() { continue; }
                    let connection = gw.background_clone();
                    gw.lifecycle.spawn(async move {
                        if let Err(error) = connection.sync_provider_inventory(&provider.id, ModelChangeSource::AutomaticSync).await {
                            tracing::warn!(provider_id = %provider.id, error = %error, "automatic model inventory synchronization failed");
                        }
                    });
                }
            }
        });
        Ok(())
    }
}
