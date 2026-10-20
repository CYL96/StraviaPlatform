use super::*;

const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
// 存储只能在活跃操作的完整时限之后回收租约。
const STALE_REFRESH_AGE: std::time::Duration = std::time::Duration::from_secs(125);

pub(crate) fn provider_oauth_needs_refresh(credential: &OAuthCredential) -> bool {
    credential
        .refresh_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
        && (matches!(credential.status.as_str(), "refreshing" | "error")
            || credential
                .expires_at
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .and_then(crate::admin::auth_data::parse_datetime_utc)
                .is_some_and(|expires| {
                    expires <= chrono::Utc::now() + chrono::Duration::seconds(60)
                }))
}

type RefreshKey = (String, String, i64);
type SharedRefreshResult = Result<OAuthCredential, std::sync::Arc<anyhow::Error>>;

#[derive(Default)]
pub(crate) struct OAuthRefreshRuntime {
    flights: parking_lot::Mutex<
        std::collections::HashMap<
            RefreshKey,
            tokio::sync::watch::Receiver<Option<SharedRefreshResult>>,
        >,
    >,
}

struct RefreshFlight {
    runtime: std::sync::Arc<OAuthRefreshRuntime>,
    key: RefreshKey,
}

impl Drop for RefreshFlight {
    fn drop(&mut self) {
        self.runtime.flights.lock().remove(&self.key);
    }
}

#[derive(Debug)]
struct SharedRefreshError(std::sync::Arc<anyhow::Error>);

impl std::fmt::Display for SharedRefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for SharedRefreshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}

struct OAuthRefreshLease {
    gateway: Gateway,
    provider_id: String,
    connection_id: String,
    prepared: crate::plugin::execution::PreparedVendorExecution,
    expected_version: i32,
    armed: bool,
}

impl OAuthRefreshLease {
    fn new(
        gateway: Gateway,
        provider_id: String,
        connection_id: String,
        prepared: crate::plugin::execution::PreparedVendorExecution,
        expected_version: i32,
    ) -> Self {
        Self {
            gateway,
            provider_id,
            connection_id,
            prepared,
            expected_version,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for OAuthRefreshLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let gateway = self.gateway.clone();
        let provider_id = self.provider_id.clone();
        let connection_id = self.connection_id.clone();
        let prepared = self.prepared.clone();
        let expected_version = self.expected_version;
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                let Ok(_fence) = prepared.write_fence().await else {
                    return;
                };
                let store = gateway.storage.oauth_credentials();
                match store.get(&provider_id).await {
                    Ok(Some(current)) if current.connection_id == connection_id => {}
                    Ok(_) => return,
                    Err(error) => {
                        tracing::warn!(
                            provider_id,
                            error = %error,
                            "Failed to read OAuth connection while releasing refresh lease"
                        );
                        return;
                    }
                }
                if let Err(error) = gateway
                    .storage
                    .oauth_credentials()
                    .cancel_refresh(&provider_id, expected_version)
                    .await
                {
                    tracing::warn!(
                        provider_id,
                        error = %error,
                        "Failed to release cancelled OAuth refresh lease"
                    );
                }
            });
        }
    }
}

impl AdminService {
    pub(in crate::admin) async fn sync_provider_runtime_fields(
        &self,
        provider: &Provider,
        _credential: &StoredCredential,
    ) -> anyhow::Result<Provider> {
        let vendor_id = provider
            .vendor
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("provider vendor is missing"))?;
        let (_, operation, _) = self.gw.vendor_plugins.acquire(vendor_id)?;
        let _permit = operation.write_permit().await?;
        let current = self
            .gw
            .storage
            .providers()
            .get(&provider.id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("provider was removed"))?;
        anyhow::ensure!(
            crate::admin::provider_connection::same_provider_generation(&current, provider),
            "provider changed while binding OAuth credentials"
        );
        let updated = self
            .gw
            .storage
            .providers()
            .update(
                &provider.id,
                UpdateProvider {
                    api_key: Some(String::new()),
                    auth_mode: Some("oauth".to_string()),
                    is_enabled: Some(provider.is_enabled),
                    ..Default::default()
                },
            )
            .await?;
        Ok(updated)
    }

    pub(crate) async fn force_refresh_provider_oauth(
        &self,
        provider_id: &str,
    ) -> anyhow::Result<OAuthCredential> {
        self.force_refresh_provider_oauth_with_context(
            provider_id,
            None,
            stravia_runtime_contract::CancellationToken::new(),
            stravia_runtime_contract::Deadline::fixed(std::time::Instant::now() + REFRESH_TIMEOUT),
            false,
        )
        .await
    }

    pub(crate) async fn recover_provider_auth_with_lease(
        &self,
        provider_id: &str,
        pinned: &crate::plugin::execution::PreparedVendorExecution,
        cancellation: stravia_runtime_contract::CancellationToken,
        deadline: stravia_runtime_contract::Deadline,
    ) -> anyhow::Result<()> {
        if pinned.oauth_connection_id().is_some() {
            self.force_refresh_provider_oauth_with_context(
                provider_id,
                Some(pinned),
                cancellation,
                deadline,
                true,
            )
            .await?;
            return Ok(());
        }

        let context = crate::plugin::VendorCallContext::new(cancellation, deadline);
        let prepared = self
            .gw
            .prepare_vendor_execution_with_lease(
                pinned,
                provider_id,
                None,
                stravia_vendor_sdk::Operation::Auth,
                &context,
            )
            .await?;
        let credential_version = prepared.credential_version();
        let execution = self
            .gw
            .execute_prepared_vendor(
                prepared,
                crate::plugin::VendorRequest::Auth(stravia_vendor_sdk::AuthRequest {
                    step: stravia_vendor_sdk::AuthStep::Refresh,
                }),
                &context,
            )
            .await;
        let execution = match execution {
            Ok(execution) => execution,
            Err(error) => {
                // 仅明确凭据拒绝才是失效证据。
                if crate::plugin::execution::is_credential_rejection(&error) {
                    self.gw
                        .mark_provider_credential_invalid(provider_id, credential_version)
                        .await;
                }
                return Err(error);
            }
        };
        let publication = execution.publication.write_fence().await?;
        anyhow::ensure!(
            matches!(
                execution.output,
                stravia_vendor_sdk::OperationOutput::Auth(
                    stravia_vendor_sdk::AuthResponse::Credentials { .. }
                )
            ),
            "vendor returned an invalid authentication refresh result"
        );
        // ADR-0073：vendor 侧刷新产出了被上游接受的新凭据——清除失效。
        self.gw.clear_provider_credential_invalid(provider_id).await;
        drop(publication);
        Ok(())
    }

    pub(crate) fn refresh_provider_oauth_with_lease<'a>(
        &'a self,
        provider_id: &'a str,
        pinned: &'a crate::plugin::execution::PreparedVendorExecution,
        cancellation: stravia_runtime_contract::CancellationToken,
        deadline: stravia_runtime_contract::Deadline,
        rejected: bool,
    ) -> futures::future::BoxFuture<'a, anyhow::Result<OAuthCredential>> {
        self.force_refresh_provider_oauth_with_context(
            provider_id,
            Some(pinned),
            cancellation,
            deadline,
            rejected,
        )
    }

    // 仅装箱刷新慢路径，切断 Auth 准备与刷新之间的 Send 递归。
    fn force_refresh_provider_oauth_with_context<'a>(
        &'a self,
        provider_id: &'a str,
        pinned: Option<&'a crate::plugin::execution::PreparedVendorExecution>,
        cancellation: stravia_runtime_contract::CancellationToken,
        deadline: stravia_runtime_contract::Deadline,
        rejected: bool,
    ) -> futures::future::BoxFuture<'a, anyhow::Result<OAuthCredential>> {
        Box::pin(async move {
            use stravia_vendor_runtime::RuntimeError;
            let provider = self.get_provider(provider_id).await?;
            let credential = self
                .gw
                .storage
                .oauth_credentials()
                .get(provider_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("provider OAuth credential not found"))?;
            if let Some(pinned) = pinned {
                anyhow::ensure!(
                    pinned.vendor_id() == provider.vendor.as_deref().unwrap_or_default()
                        && pinned.oauth_connection_id() == Some(credential.connection_id.as_str())
                        && pinned.credential_version().provider_revision == provider.revision,
                    "provider changed before OAuth refresh"
                );
                // 延迟的 401 可能拒绝的是其他 flight 已替换的旧 token。
                // 单纯版本变化不是刷新成功证据：取消、租约回收也会递增版本。
                if credential.status == "connected"
                    && !credential.access_token.trim().is_empty()
                    && !is_expired_at(credential.expires_at.as_deref())
                    && credential.last_error.is_none()
                    && pinned
                        .provider()
                        .credentials
                        .get("access_token")
                        .and_then(Value::as_str)
                        .is_some_and(|token| token != credential.access_token)
                {
                    return Ok(credential);
                }
            }
            if cancellation.is_cancelled() || self.gw.lifecycle.cancellation.is_cancelled() {
                return Err(RuntimeError::Cancelled.into());
            }
            if deadline.is_exceeded() {
                return Err(RuntimeError::DeadlineExceeded.into());
            }
            let key = (
                provider_id.to_owned(),
                credential.connection_id.clone(),
                provider.revision,
            );
            let mut receiver = {
                let mut flights = self.gw.oauth_refresh.flights.lock();
                if let Some(receiver) = flights.get(&key) {
                    receiver.clone()
                } else {
                    let (sender, receiver) = tokio::sync::watch::channel(None);
                    flights.insert(key.clone(), receiver.clone());
                    let gw = self.gw.background_clone();
                    let pinned = pinned.cloned();
                    let flight = RefreshFlight {
                        runtime: self.gw.oauth_refresh.clone(),
                        key,
                    };
                    self.gw.lifecycle.spawn(async move {
                        let admin = gw.admin();
                        let result = admin
                            .run_provider_oauth_refresh(
                                &provider,
                                credential,
                                pinned.as_ref(),
                                gw.lifecycle.cancellation.clone(),
                                stravia_runtime_contract::Deadline::fixed(
                                    std::time::Instant::now() + REFRESH_TIMEOUT,
                                ),
                                rejected,
                            )
                            .await
                            .map_err(std::sync::Arc::new);
                        sender.send_replace(Some(result));
                        drop(flight);
                    });
                    receiver
                }
            };
            loop {
                if let Some(result) = receiver.borrow().clone() {
                    return result.map_err(|error| {
                        // anyhow 下转不会遍历任意 source() 链；
                        // 将原始 runtime 类型作为 context 保留分类信息。
                        let runtime = error.downcast_ref::<RuntimeError>().cloned();
                        let shared = anyhow::Error::new(SharedRefreshError(error));
                        match runtime {
                            Some(runtime) => shared.context(runtime),
                            None => shared,
                        }
                    });
                }
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(RuntimeError::Cancelled.into()),
                    () = deadline.wait() => return Err(RuntimeError::DeadlineExceeded.into()),
                    changed = receiver.changed() => {
                        changed.map_err(|_| anyhow::Error::from(RuntimeError::Cancelled))?;
                    }
                }
            }
        })
    }

    async fn run_provider_oauth_refresh(
        &self,
        provider: &Provider,
        mut credential: OAuthCredential,
        pinned: Option<&crate::plugin::execution::PreparedVendorExecution>,
        cancellation: stravia_runtime_contract::CancellationToken,
        deadline: stravia_runtime_contract::Deadline,
        rejected: bool,
    ) -> anyhow::Result<OAuthCredential> {
        let provider_id = provider.id.as_str();
        let vendor_id = provider
            .vendor
            .as_deref()
            .map(str::trim)
            .filter(|vendor| !vendor.is_empty())
            .ok_or_else(|| anyhow::anyhow!("provider vendor is missing"))?
            .to_owned();
        let oauth_store = self.gw.storage.oauth_credentials();
        let context = crate::plugin::VendorCallContext::new(cancellation.clone(), deadline.clone());
        // 获取存储租约前固定组件和连接；Auth 准备不得进入非 Auth 预刷新 gate。
        let prepared = match pinned {
            Some(pinned) => {
                self.gw
                    .prepare_vendor_execution_with_lease(
                        pinned,
                        provider_id,
                        None,
                        stravia_vendor_sdk::Operation::Auth,
                        &context,
                    )
                    .await?
            }
            None => {
                self.gw
                    .prepare_vendor_execution(
                        provider_id,
                        None,
                        stravia_vendor_sdk::Operation::Auth,
                        &context,
                    )
                    .await?
            }
        };
        anyhow::ensure!(
            prepared.vendor_id() == vendor_id
                && prepared.oauth_connection_id() == Some(credential.connection_id.as_str())
                && prepared.credential_version().provider_revision == provider.revision,
            "provider changed before OAuth refresh"
        );
        if credential
            .refresh_token
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            // ADR-0073：无 refresh token 意味着被拒的 access token 无法恢复，
            // 恢复路径到此即耗尽。
            if rejected {
                let _fence = prepared.write_fence().await?;
                self.gw
                    .mark_provider_credential_invalid(
                        provider_id,
                        crate::db::models::ProviderCredentialVersion {
                            provider_revision: provider.revision,
                            oauth_status_version: Some(credential.status_version),
                        },
                    )
                    .await;
            }
            return Err(stravia_vendor_runtime::RuntimeError::from_guest(
                stravia_vendor_sdk::ErrorKind::Auth,
                "provider OAuth refresh token is missing".to_owned(),
                None,
            )
            .into());
        }
        let locked = loop {
            let current = oauth_store
                .get(provider_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("provider OAuth credential disappeared"))?;
            anyhow::ensure!(
                current.connection_id == credential.connection_id,
                "provider OAuth connection changed before refresh"
            );
            if current.status != "refreshing" {
                if current.status_version != credential.status_version {
                    if current.status == "connected"
                        && current.last_error.as_deref()
                            == Some("refresh lease expired; retrying is allowed")
                    {
                        // 租约回收不代表刷新成功，旧 token 仍需实际交换一次。
                        credential.status_version = current.status_version;
                    } else {
                        let current_provider = self
                            .gw
                            .storage
                            .providers()
                            .get(provider_id)
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!("provider was removed during OAuth refresh")
                            })?;
                        anyhow::ensure!(
                            crate::admin::provider_connection::same_provider_generation(
                                &current_provider,
                                provider,
                            ),
                            "provider changed during OAuth refresh"
                        );
                        if current_provider.credential_invalid() {
                            return Err(stravia_vendor_runtime::RuntimeError::from_guest(
                                stravia_vendor_sdk::ErrorKind::Auth,
                                "concurrent provider OAuth refresh rejected the credential"
                                    .to_owned(),
                                None,
                            )
                            .into());
                        }
                        if current.status == "connected"
                            && !current.access_token.trim().is_empty()
                            && !is_expired_at(current.expires_at.as_deref())
                            && (current.last_refresh_at != credential.last_refresh_at
                                || current.access_token != credential.access_token
                                || current.refresh_token != credential.refresh_token
                                || current.expires_at != credential.expires_at)
                        {
                            return Ok(current);
                        }
                        // 存储保留失败诊断而非上游错误类型；没有明确失效证据时仍可重试，
                        // 不能据此推断 HTTP 状态或 Retry-After。
                        return Err(stravia_vendor_runtime::RuntimeError::from_guest(
                            stravia_vendor_sdk::ErrorKind::upstream(
                                Some(stravia_runtime_contract::protocol::ir::AiErrorKind::ServiceUnavailable),
                                None,
                            ),
                            "concurrent provider OAuth refresh did not publish fresh credentials".to_owned(),
                            None,
                        ).into());
                    }
                }
                if let Some(locked) = oauth_store
                    .try_begin_refresh(provider_id, current.status_version)
                    .await?
                {
                    break locked;
                }
            } else {
                // 仅回收超过共享操作完整时限的租约。
                oauth_store
                    .recover_stale_refreshing(STALE_REFRESH_AGE)
                    .await?;
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(stravia_vendor_runtime::RuntimeError::Cancelled.into()),
                () = deadline.wait() => return Err(stravia_vendor_runtime::RuntimeError::DeadlineExceeded.into()),
                _ = prepared.cancelled() => return Err(stravia_vendor_runtime::RuntimeError::Cancelled.into()),
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
            }
            // 其他进程持有 flight 时仅轮询存储，不重试 token endpoint。
        };
        let mut refresh_lease = OAuthRefreshLease::new(
            self.gw.clone(),
            provider_id.to_owned(),
            credential.connection_id.clone(),
            prepared.clone(),
            locked.status_version,
        );
        let provider_fingerprint = provider.clone();
        let execution = self
            .gw
            .execute_prepared_vendor(
                prepared.clone(),
                crate::plugin::VendorRequest::Auth(stravia_vendor_sdk::AuthRequest {
                    step: stravia_vendor_sdk::AuthStep::Refresh,
                }),
                &context,
            )
            .await;
        let execution = match execution {
            Ok(execution) => execution,
            Err(error) => {
                let Ok(fence) = prepared.write_fence().await else {
                    return Err(error);
                };
                let current_provider = self.gw.storage.providers().get(provider_id).await?;
                let current_credential = oauth_store.get(provider_id).await?;
                if !current_provider.as_ref().is_some_and(|current| {
                    crate::admin::provider_connection::same_provider_generation(current, provider)
                }) || !current_credential.as_ref().is_some_and(|current| {
                    current.connection_id == credential.connection_id
                        && current.status_version == locked.status_version
                }) {
                    return Err(error);
                }
                // fail_refresh 会递增凭据版本，失效证据须在此之前写入。
                if crate::plugin::execution::is_credential_rejection(&error) {
                    self.gw
                        .mark_provider_credential_invalid(
                            provider_id,
                            crate::db::models::ProviderCredentialVersion {
                                provider_revision: provider.revision,
                                oauth_status_version: Some(locked.status_version),
                            },
                        )
                        .await;
                }
                oauth_store
                    .fail_refresh(provider_id, locked.status_version, &error.to_string())
                    .await?;
                refresh_lease.disarm();
                drop(fence);
                return Err(error.context("refresh provider OAuth credential"));
            }
        };
        let publication = execution.publication.write_fence().await?;
        let stravia_vendor_sdk::OperationOutput::Auth(
            response @ stravia_vendor_sdk::AuthResponse::Credentials { .. },
        ) = execution.output
        else {
            anyhow::bail!("vendor returned an invalid OAuth refresh result")
        };
        let mut bundle = super::credential_bundle_from_response(response)?;
        if bundle
            .refresh_token
            .as_deref()
            .is_none_or(|token| token.trim().is_empty())
        {
            bundle.refresh_token = credential.refresh_token.take();
            if let (Some(raw), Some(token)) =
                (bundle.raw.as_object_mut(), bundle.refresh_token.as_ref())
            {
                raw.insert("refresh_token".to_owned(), Value::String(token.clone()));
            }
        }
        let current_provider = self
            .gw
            .storage
            .providers()
            .get(provider_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("provider was removed during OAuth refresh"))?;
        anyhow::ensure!(
            crate::admin::provider_connection::same_provider_generation(
                &current_provider,
                &provider_fingerprint
            ),
            "provider changed during OAuth refresh"
        );
        let current_credential = oauth_store.get(provider_id).await?.ok_or_else(|| {
            anyhow::anyhow!("provider OAuth credential was removed during refresh")
        })?;
        anyhow::ensure!(
            current_credential.connection_id == credential.connection_id
                && current_credential.status_version == locked.status_version,
            "provider OAuth connection changed during refresh"
        );
        oauth_store
            .complete_refresh(
                provider_id,
                locked.status_version,
                upsert_credential_from_bundle(&vendor_id, &credential.scheme, &bundle),
            )
            .await?;
        // ADR-0073：刷新成功=上游已接受新凭据，清除失效标记。
        self.gw.clear_provider_credential_invalid(provider_id).await;
        self.gw
            .vendor_plugins
            .store
            .recovered(provider_id, "credentials")
            .await?;
        refresh_lease.disarm();
        let refreshed = oauth_store
            .get(provider_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("refreshed OAuth credential was not persisted"))?;
        drop(publication);
        Ok(refreshed)
    }

    pub async fn refresh_oauth_providers(&self) -> anyhow::Result<usize> {
        let oauth_store = self.gw.storage.oauth_credentials();
        let recovered = oauth_store
            .recover_stale_refreshing(STALE_REFRESH_AGE)
            .await?;
        if recovered > 0 {
            tracing::info!("recovered {recovered} stale refreshing OAuth credentials");
        }
        let expiring = oauth_store
            .list_expiring(std::time::Duration::from_secs(300))
            .await?;
        let mut refreshed = 0;
        for credential in expiring {
            let Some(provider) = self
                .gw
                .storage
                .providers()
                .get(&credential.provider_id)
                .await?
            else {
                continue;
            };
            if !provider.is_enabled || provider.credential_invalid() {
                continue;
            }
            if credential
                .refresh_token
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                continue;
            }
            match self
                .force_refresh_provider_oauth(&credential.provider_id)
                .await
            {
                Ok(_) => refreshed += 1,
                Err(error) => tracing::warn!(
                    provider_id = credential.provider_id,
                    %error,
                    "background OAuth refresh failed"
                ),
            }
        }
        Ok(refreshed)
    }

    pub(in crate::admin) async fn reconnect_provider_oauth_record(
        &self,
        id: &str,
    ) -> anyhow::Result<ProviderOAuthStatusData> {
        let provider = self.get_provider(id).await?;
        let credential = self.force_refresh_provider_oauth(id).await?;
        Ok(build_provider_oauth_status_from_credential(
            &provider,
            provider.vendor.as_deref().unwrap_or_default(),
            &credential,
        ))
    }
}
