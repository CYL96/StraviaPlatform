use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use stravia_vendor_common::common;
use stravia_vendor_sdk::{ErrorKind, GuestHost, PluginError, ProviderSnapshot};

const MAX_SESSIONS: usize = 256;
const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

#[derive(Default, Deserialize, Serialize)]
struct PrivateState {
    // 宿主按 vendor/provider 隔离；这是可淘汰的路由亲和缓存，不是对话历史或授权状态。
    // GuestHost 没有 CAS，并发未命中可能覆盖缓存项；完整消息仍随每次请求发送。
    // 顺序只代表创建先后；命中时不改顺序，避免每次请求都写私有状态。
    sessions: VecDeque<SessionEntry>,
}

#[derive(Deserialize, Serialize)]
struct SessionEntry {
    affinity: String,
    native_session: String,
}

pub(crate) fn apply(
    host: &GuestHost,
    provider: &ProviderSnapshot,
    headers: &mut Vec<(String, String)>,
) -> Result<(), PluginError> {
    let mut ids = NativeIds::now()?;
    let session = resolve_session(
        common::session_affinity(provider),
        || host.read_private_state(),
        |bytes| host.write_private_state(bytes),
        || ids.create("ses_", true),
    )?;
    let message = ids.create("msg_", false);

    // 新亲和映射必须先落盘，失败时不把尚不可恢复的身份发给上游。
    crate::set_header(headers, "user-agent", crate::CLIENT_USER_AGENT.to_owned());
    crate::set_header(headers, "x-opencode-client", "cli".into());
    crate::set_header(headers, "x-opencode-project", "global".into());
    crate::set_header(headers, "x-opencode-session", session);
    crate::set_header(headers, "x-opencode-request", message);
    Ok(())
}

fn resolve_session(
    affinity: Option<&str>,
    read: impl FnOnce() -> Result<Option<Vec<u8>>, PluginError>,
    write: impl FnOnce(&[u8]) -> Result<(), PluginError>,
    create: impl FnOnce() -> String,
) -> Result<String, PluginError> {
    let Some(affinity) = affinity else {
        // 发现和独立请求没有可恢复的链路，不读取或污染提供商持久状态。
        return Ok(create());
    };
    let mut state = match read()? {
        None => PrivateState::default(),
        Some(bytes) => serde_json::from_slice::<PrivateState>(&bytes).map_err(|err| {
            common::plugin_error(
                ErrorKind::Invalid,
                format!("decode OpenCode native identity state: {err}"),
            )
        })?,
    };
    if state.sessions.len() > MAX_SESSIONS {
        return Err(common::plugin_error(
            ErrorKind::Invalid,
            "OpenCode native identity state exceeds session capacity",
        ));
    }
    if let Some(entry) = state
        .sessions
        .iter()
        .find(|entry| entry.affinity == affinity)
    {
        return Ok(entry.native_session.clone());
    }

    let native_session = create();
    if state.sessions.len() == MAX_SESSIONS {
        // 容量满时只淘汰创建最早的一项，不能打散其余活跃链路的上游身份。
        state.sessions.pop_front();
    }
    state.sessions.push_back(SessionEntry {
        affinity: affinity.to_owned(),
        native_session: native_session.clone(),
    });
    let bytes = serde_json::to_vec(&state).map_err(|err| {
        common::plugin_error(
            ErrorKind::Trapped,
            format!("encode OpenCode native identity state: {err}"),
        )
    })?;
    write(&bytes)?;
    Ok(native_session)
}

struct NativeIds {
    timestamp_ms: u64,
    counter: u16,
}

impl NativeIds {
    fn now() -> Result<Self, PluginError> {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| {
                common::plugin_error(
                    ErrorKind::Trapped,
                    format!("read OpenCode native identity UTC clock: {err}"),
                )
            })?;
        let timestamp_ms = u64::try_from(elapsed.as_millis()).map_err(|err| {
            common::plugin_error(
                ErrorKind::Trapped,
                format!("encode OpenCode native identity UTC milliseconds: {err}"),
            )
        })?;
        Ok(Self {
            timestamp_ms,
            counter: 0,
        })
    }

    fn create(&mut self, prefix: &str, descending: bool) -> String {
        // 官方计数从 1 开始；本操作最多生成 session/message 两项，随机尾部负责跨操作去重。
        self.counter += 1;
        let random = uuid::Uuid::new_v4();
        let bytes = random.as_bytes();
        // UUID 的第 6/8 字节含固定版本/变体位，排除它们后仍有 14 个随机字节。
        let mut tail = [0_u8; 14];
        let mut index = 0;
        for (position, byte) in bytes.iter().enumerate() {
            if position != 6 && position != 8 {
                tail[index] = *byte;
                index += 1;
            }
        }
        encode_id(prefix, self.timestamp_ms, self.counter, descending, &tail)
    }
}

fn encode_id(
    prefix: &str,
    timestamp_ms: u64,
    counter: u16,
    descending: bool,
    random: &[u8; 14],
) -> String {
    // 用宽整数保留官方 BigInt 语义，再取低 48 位，而不是截断宿主亲和键。
    let current = u128::from(timestamp_ms) * 4096 + u128::from(counter);
    let value = if descending { !current } else { current };
    let mut id = String::with_capacity(prefix.len() + 26);
    id.push_str(prefix);
    for shift in [40, 32, 24, 16, 8, 0] {
        // String 的格式写入不会失败；逐字节输出确保与官方大端编码一致。
        write!(&mut id, "{:02x}", (value >> shift) & 0xff).expect("writing to String");
    }
    for byte in random {
        id.push(char::from(BASE62[usize::from(*byte) % BASE62.len()]));
    }
    id
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Default)]
    struct Storage {
        bytes: RefCell<Option<Vec<u8>>>,
        reads: Cell<usize>,
        writes: Cell<usize>,
    }

    impl Storage {
        fn session(&self, affinity: Option<&str>, create: impl FnOnce() -> String) -> String {
            resolve_session(
                affinity,
                || {
                    self.reads.set(self.reads.get() + 1);
                    Ok(self.bytes.borrow().clone())
                },
                |bytes| {
                    self.writes.set(self.writes.get() + 1);
                    *self.bytes.borrow_mut() = Some(bytes.to_vec());
                    Ok(())
                },
                create,
            )
            .unwrap()
        }

        fn state(&self) -> PrivateState {
            serde_json::from_slice(self.bytes.borrow().as_deref().unwrap()).unwrap()
        }
    }

    fn session_at(timestamp_ms: u64) -> String {
        encode_id("ses_", timestamp_ms, 1, true, &[0; 14])
    }

    #[test]
    fn official_timestamp_vectors_preserve_direction_counter_and_low_48_bits() {
        let random = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];
        for (timestamp, counter, ascending, descending) in [
            (0, 1, "000000000001", "fffffffffffe"),
            (1, 1, "000000001001", "ffffffffeffe"),
            (1, 2, "000000001002", "ffffffffeffd"),
            (0x123456789ab, 1, "3456789ab001", "cba987654ffe"),
        ] {
            assert_eq!(
                encode_id("msg_", timestamp, counter, false, &random),
                format!("msg_{ascending}0123456789ABCD")
            );
            assert_eq!(
                encode_id("ses_", timestamp, counter, true, &random),
                format!("ses_{descending}0123456789ABCD")
            );
        }
    }

    #[test]
    fn operation_counter_advances_between_session_and_message() {
        let mut ids = NativeIds {
            timestamp_ms: 1,
            counter: 0,
        };
        let session = ids.create("ses_", true);
        let message = ids.create("msg_", false);
        assert!(session.starts_with("ses_ffffffffeffe"));
        assert!(message.starts_with("msg_000000001002"));
        for id in [&session, &message] {
            assert!(id[16..].bytes().all(|byte| BASE62.contains(&byte)));
        }
    }

    #[test]
    fn serialized_restart_reuses_affinity_without_generating_or_writing() {
        let original = Storage::default();
        let first = original.session(Some("chain-a"), || session_at(100));
        assert_eq!(original.writes.get(), 1);
        let restarted = Storage {
            bytes: RefCell::new(original.bytes.into_inner()),
            ..Storage::default()
        };
        assert_eq!(
            restarted.session(Some("chain-a"), || panic!("must reuse persisted session")),
            first
        );
        assert_eq!(restarted.reads.get(), 1);
        assert_eq!(restarted.writes.get(), 0);
    }

    #[test]
    fn distinct_affinities_keep_separate_native_sessions() {
        let storage = Storage::default();
        let a = storage.session(Some("chain-a"), || session_at(100));
        let b = storage.session(Some("chain-b"), || session_at(200));
        assert_ne!(a, b);
        assert_eq!(storage.session(Some("chain-a"), || panic!("existing a")), a);
        assert_eq!(storage.session(Some("chain-b"), || panic!("existing b")), b);
        assert_eq!(storage.writes.get(), 2);
    }

    #[test]
    fn full_capacity_evicts_only_oldest_and_hits_do_not_write() {
        let storage = Storage::default();
        for index in 0..MAX_SESSIONS {
            storage.session(Some(&format!("chain-{index}")), || session_at(index as u64));
        }
        storage.session(Some("chain-0"), || panic!("existing oldest"));
        assert_eq!(storage.writes.get(), MAX_SESSIONS);
        let last = storage.session(Some("new-chain"), || session_at(1000));
        let state = storage.state();
        assert_eq!(state.sessions.len(), MAX_SESSIONS);
        assert_eq!(state.sessions.front().unwrap().affinity, "chain-1");
        assert_eq!(state.sessions.back().unwrap().native_session, last);
        for index in 1..MAX_SESSIONS {
            assert_eq!(
                storage.session(Some(&format!("chain-{index}")), || panic!(
                    "surviving chain"
                )),
                session_at(index as u64)
            );
        }
        assert_eq!(storage.writes.get(), MAX_SESSIONS + 1);
    }

    #[test]
    fn missing_affinity_never_reads_or_writes_provider_state() {
        let storage = Storage::default();
        storage.session(Some("active-chain"), || session_at(100));
        let before = storage.bytes.borrow().clone();
        assert_eq!(storage.session(None, || session_at(200)), session_at(200));
        assert_eq!(storage.session(None, || session_at(300)), session_at(300));
        assert_eq!(*storage.bytes.borrow(), before);
        assert_eq!(storage.reads.get(), 1);
        assert_eq!(storage.writes.get(), 1);
    }

    #[test]
    fn new_mapping_is_persisted_before_session_is_returned() {
        let persisted = RefCell::new(None);
        let expected = session_at(100);
        let returned = resolve_session(
            Some("chain"),
            || Ok(None),
            |bytes| {
                let state: PrivateState = serde_json::from_slice(bytes).unwrap();
                assert_eq!(state.sessions[0].native_session, expected);
                *persisted.borrow_mut() = Some(bytes.to_vec());
                Ok(())
            },
            || expected.clone(),
        )
        .unwrap();
        assert_eq!(returned, expected);
        assert!(persisted.borrow().is_some());
    }

    #[test]
    fn read_decode_and_write_failures_do_not_produce_a_session() {
        let read_failure = resolve_session(
            Some("chain"),
            || Err(common::plugin_error(ErrorKind::Trapped, "read failed")),
            |_| panic!("must not write after failed read"),
            || panic!("must not create after failed read"),
        )
        .unwrap_err();
        assert_eq!(read_failure.message, "read failed");

        let decode_failure = resolve_session(
            Some("chain"),
            || Ok(Some(b"not JSON".to_vec())),
            |_| panic!("must not overwrite invalid state"),
            || panic!("must not create after failed decode"),
        )
        .unwrap_err();
        assert!(matches!(decode_failure.kind, ErrorKind::Invalid));

        let write_failure = resolve_session(
            Some("chain"),
            || Ok(None),
            |_| Err(common::plugin_error(ErrorKind::Trapped, "write failed")),
            || session_at(100),
        )
        .unwrap_err();
        assert_eq!(write_failure.message, "write failed");
    }
}
