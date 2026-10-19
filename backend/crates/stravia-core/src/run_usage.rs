//! In-memory execution accounting, independent of diagnostic persistence.
use std::sync::Arc;

use parking_lot::Mutex;
use stravia_runtime_contract::protocol::ir::Usage;

#[derive(Clone, Default)]
pub(crate) struct ConfirmedUsageLedger(Arc<Mutex<Option<Usage>>>);

impl ConfirmedUsageLedger {
    pub(crate) fn confirm(&self, usage: &Usage) {
        let mut total = self.0.lock();
        match total.as_mut() {
            Some(total) => add_usage(total, usage),
            None => *total = Some(usage.clone()),
        }
    }

    pub(crate) fn snapshot(&self) -> Option<Usage> {
        self.0.lock().clone()
    }
}

pub(crate) fn add_usage(total: &mut Usage, usage: &Usage) {
    total.prompt_tokens = total.prompt_tokens.saturating_add(usage.prompt_tokens);
    total.required_components_known &= usage.required_components_known;
    total.completion_tokens = total
        .completion_tokens
        .saturating_add(usage.completion_tokens);
    total.total_tokens = total.total_tokens.saturating_add(usage.total_tokens);
    total.cache_read_tokens = sum_optional(total.cache_read_tokens, usage.cache_read_tokens);
    total.cache_creation_tokens =
        sum_optional(total.cache_creation_tokens, usage.cache_creation_tokens);
    total.reasoning_tokens = sum_optional(total.reasoning_tokens, usage.reasoning_tokens);
    match (&mut total.server_tool_use, &usage.server_tool_use) {
        (Some(total), Some(usage)) => {
            total.web_search_requests = total
                .web_search_requests
                .saturating_add(usage.web_search_requests);
            total.web_fetch_requests = total
                .web_fetch_requests
                .saturating_add(usage.web_fetch_requests);
        }
        (None, Some(usage)) => total.server_tool_use = Some(usage.clone()),
        _ => {}
    }
}

fn sum_optional(left: Option<u32>, right: Option<u32>) -> Option<u32> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        _ => None,
    }
}
