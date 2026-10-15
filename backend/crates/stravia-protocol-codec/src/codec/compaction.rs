//! Native compaction control classification on a decoded canonical request.
//!
//! Moved out of `stravia-core::compaction`: the persisted store stays host-side
//! while request classification is pure wire semantics shared with plugins.

use stravia_runtime_contract::protocol::ir::{AiItem, AiRequest};

/// Whether a decoded request asks for native compaction.
///
/// A unified compaction control or an explicit trigger activates compaction.
pub fn native_compaction_requested(request: &AiRequest) -> bool {
    if request.items.iter().any(AiItem::is_compaction_trigger) {
        return true;
    }
    request.parameters.compaction.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stravia_runtime_contract::protocol::ir::CompactionControl;

    #[test]
    fn unified_compaction_control_activates_with_or_without_threshold() {
        let mut request = AiRequest::new("model", vec![]);
        assert!(!native_compaction_requested(&request));
        for threshold_tokens in [None, Some(100_000)] {
            request.parameters.compaction = Some(CompactionControl { threshold_tokens });
            assert!(native_compaction_requested(&request));
        }
    }
}
