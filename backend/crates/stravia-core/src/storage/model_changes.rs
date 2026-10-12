use crate::provider_models::{ModelChangePage, ModelChangeQuery, ModelChangeRecord};
use anyhow::Context;
use base64::Engine;

pub(super) fn cutoff() -> String {
    (chrono::Utc::now() - chrono::Duration::days(90))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(super) fn cursor(query: &ModelChangeQuery) -> anyhow::Result<Option<(String, String)>> {
    query
        .cursor
        .as_deref()
        .map(|cursor| {
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(cursor)
                .context("invalid model changes cursor")?;
            serde_json::from_slice(&bytes).context("invalid model changes cursor")
        })
        .transpose()
}

pub(super) fn page(
    mut records: Vec<ModelChangeRecord>,
    limit: usize,
) -> anyhow::Result<ModelChangePage> {
    let next_cursor = if records.len() > limit {
        records.truncate(limit);
        let last = records.last().context("empty model changes page")?;
        Some(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&(&last.checked_at, &last.id))?),
        )
    } else {
        None
    };
    Ok(ModelChangePage {
        records,
        next_cursor,
    })
}

pub(super) fn matches(
    record: &ModelChangeRecord,
    query: &ModelChangeQuery,
    needle: Option<&str>,
) -> bool {
    query
        .provider_id
        .as_ref()
        .is_none_or(|id| id == &record.provider_id)
        && query
            .source
            .as_ref()
            .is_none_or(|source| source == &record.source)
        && query.change_type.as_ref().is_none_or(|kind| {
            record.changes.iter().any(|item| item.types.contains(kind))
                || (*kind == crate::provider_models::ModelChangeType::SyncFailed
                    && record.error.is_some())
        })
        && needle.is_none_or(|q| {
            record.changes.iter().any(|item| {
                item.name.to_lowercase().contains(q) || item.model_id.to_lowercase().contains(q)
            })
        })
}
