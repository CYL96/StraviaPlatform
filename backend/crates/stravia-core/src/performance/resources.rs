//! 托管资源的逻辑占用。始终维护计数，使后期开启 Debug 也能看到现存对象。
//! 字节来自调用方已有预算或缓冲长度，不代表 allocator 或进程 RSS。

use std::collections::BTreeMap;
use std::sync::{
    Arc, LazyLock, Weak,
    atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;

static RESOURCES: LazyLock<Mutex<Vec<Weak<Usage>>>> = LazyLock::new(|| Mutex::new(Vec::new()));

struct Usage {
    resource: &'static str,
    domain: &'static str,
    items: AtomicU64,
    bytes: AtomicU64,
    observed_at: Mutex<Option<u64>>,
}

#[derive(Clone)]
pub(crate) struct ResourceUsage(Arc<Usage>);

impl ResourceUsage {
    pub(crate) fn new(resource: &'static str, domain: &'static str) -> Self {
        let usage = Arc::new(Usage {
            resource,
            domain,
            items: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            observed_at: Mutex::new(None),
        });
        let mut resources = RESOURCES.lock();
        resources.retain(|resource| resource.strong_count() > 0);
        resources.push(Arc::downgrade(&usage));
        Self(usage)
    }

    /// 外部资源只承诺最后收到的快照，不把远端状态伪装成实时宿主占用。
    /// 不要与同一实例的 lease 混用。
    pub(crate) fn set_observed(&self, items: u64, bytes: u64, observed_at: SystemTime) {
        let millis = observed_at
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        // 同一回复的数量、字节和时间必须一起读写，不能拼接不同并发回复。
        let mut observed_at = self.0.observed_at.lock();
        self.0.items.store(items, Ordering::Relaxed);
        self.0.bytes.store(bytes, Ordering::Relaxed);
        *observed_at = Some(millis.min(u128::from(u64::MAX)) as u64);
    }

    pub(crate) fn lease(&self, items: u64, bytes: u64) -> ResourceLease {
        self.0.items.fetch_add(items, Ordering::Relaxed);
        self.0.bytes.fetch_add(bytes, Ordering::Relaxed);
        ResourceLease {
            usage: self.clone(),
            items,
            bytes,
        }
    }

    #[cfg(test)]
    pub(crate) fn totals(&self) -> (u64, u64) {
        (
            self.0.items.load(Ordering::Relaxed),
            self.0.bytes.load(Ordering::Relaxed),
        )
    }
}

/// 随真实资源移动，错误、取消与拒收均通过 Drop 释放记账。
pub(crate) struct ResourceLease {
    usage: ResourceUsage,
    items: u64,
    bytes: u64,
}

impl ResourceLease {
    pub(crate) fn resize(&mut self, items: u64, bytes: u64) {
        adjust(&self.usage.0.items, self.items, items);
        adjust(&self.usage.0.bytes, self.bytes, bytes);
        self.items = items;
        self.bytes = bytes;
    }
}

fn adjust(counter: &AtomicU64, previous: u64, current: u64) {
    if current >= previous {
        counter.fetch_add(current - previous, Ordering::Relaxed);
    } else {
        counter.fetch_sub(previous - current, Ordering::Relaxed);
    }
}

impl Drop for ResourceLease {
    fn drop(&mut self) {
        self.usage.0.items.fetch_sub(self.items, Ordering::Relaxed);
        self.usage.0.bytes.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

#[derive(Clone, Serialize)]
pub(super) struct ResourcePoint {
    pub resource: &'static str,
    pub domain: &'static str,
    pub items: u64,
    pub logical_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_age_ms: Option<u64>,
}

pub(super) fn snapshot() -> Vec<ResourcePoint> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let mut totals = BTreeMap::<(&'static str, &'static str), (u64, u64, Option<u64>)>::new();
    RESOURCES.lock().retain(|source| {
        let Some(usage) = source.upgrade() else {
            return false;
        };
        let total = totals.entry((usage.resource, usage.domain)).or_default();
        let observed_at = usage.observed_at.lock();
        total.0 = total.0.saturating_add(usage.items.load(Ordering::Relaxed));
        total.1 = total.1.saturating_add(usage.bytes.load(Ordering::Relaxed));
        if let Some(observed_at) = *observed_at {
            total.2 = Some(
                total
                    .2
                    .map_or(observed_at, |previous| previous.min(observed_at)),
            );
        }
        true
    });
    totals
        .into_iter()
        .map(
            |((resource, domain), (items, logical_bytes, observed_at))| ResourcePoint {
                resource,
                domain,
                items,
                logical_bytes,
                observed_at_unix_ms: observed_at,
                observation_age_ms: observed_at.map(|at| now.saturating_sub(at)),
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::time::Duration;

    #[test]
    fn remote_snapshots_keep_count_bytes_and_time_together() {
        let usage = ResourceUsage::new("remote_snapshot_test", "atomicity");
        let barrier = Arc::new(Barrier::new(3));
        let mismatch = std::thread::scope(|scope| {
            for offset in 0..2 {
                let usage = usage.clone();
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    for round in 0..2048 {
                        let value = round * 2 + offset + 1;
                        barrier.wait();
                        usage.set_observed(
                            value,
                            value * 1024,
                            UNIX_EPOCH + Duration::from_millis(value),
                        );
                        barrier.wait();
                    }
                });
            }
            let mut mismatch = None;
            for _ in 0..2048 {
                barrier.wait();
                barrier.wait();
                // 两个写者均已提交、并等待下一轮；此处不能混合不同回复的字段。
                let point = snapshot()
                    .into_iter()
                    .find(|point| point.resource == "remote_snapshot_test")
                    .unwrap();
                if (point.logical_bytes != point.items * 1024
                    || point.observed_at_unix_ms != Some(point.items))
                    && mismatch.is_none()
                {
                    mismatch = Some((point.items, point.logical_bytes, point.observed_at_unix_ms));
                }
            }
            mismatch
        });
        assert!(
            mismatch.is_none(),
            "remote resource sample mixed replies: {mismatch:?}"
        );
    }
}
