//! 每个分段自包含：重复元数据和 payload 引用本段已写入的内容，不使用压缩。
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde_json::Value;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use crate::performance::{ResourceLease, ResourceUsage};

const CACHE_BYTES: usize = 8 * 1024 * 1024;

#[derive(serde::Deserialize)]
struct EncodedRecord<'a> {
    sequence: Value,
    recorded_at: Value,
    #[serde(borrow)]
    payload: &'a RawValue,
    #[serde(flatten)]
    metadata: serde_json::Map<String, Value>,
}

struct StoredRecord<'a> {
    fields: &'a serde_json::Map<String, Value>,
    payload: &'a RawValue,
}

impl serde::Serialize for StoredRecord<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.fields.len()))?;
        for (key, value) in self.fields {
            if key == "payload" {
                map.serialize_entry(key, self.payload)?;
            } else {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}

pub(super) struct Encoder {
    metadata: HashMap<[u8; 32], (Vec<u8>, u64)>,
    payloads: HashMap<[u8; 32], (Vec<u8>, u64)>,
    bytes: usize,
    usage: ResourceLease,
}

impl Default for Encoder {
    fn default() -> Self {
        static USAGE: std::sync::LazyLock<ResourceUsage> =
            std::sync::LazyLock::new(|| ResourceUsage::new("wire_dedup_payload", "observation"));
        Self {
            metadata: HashMap::new(),
            payloads: HashMap::new(),
            bytes: 0,
            usage: USAGE.lease(0, 0),
        }
    }
}

impl Encoder {
    pub(super) fn encode(&mut self, line: &[u8], offset: u64) -> io::Result<Vec<u8>> {
        // 入队和离线重写均已用 serde_json 编码；借用既有 payload，
        // 不再将大正文解码成 String/Value 后重新编码。
        let record: EncodedRecord<'_> = serde_json::from_slice(line).map_err(io::Error::other)?;
        let metadata = serde_json::to_vec(&record.metadata).map_err(io::Error::other)?;
        let metadata_len = metadata.len();
        let content = record.payload.get().as_bytes();
        if self
            .bytes
            .saturating_add(metadata.len())
            .saturating_add(content.len())
            > CACHE_BYTES
        {
            self.metadata.clear();
            self.payloads.clear();
            self.bytes = 0;
            self.usage.resize(0, 0);
        }
        let mut stored = serde_json::json!({
            "trace_storage":1,"sequence":record.sequence,"recorded_at":record.recorded_at
        });
        if let Some(reference) = self.intern(true, metadata.into(), offset) {
            stored["meta_ref"] = reference.into();
        } else {
            stored["meta"] = Value::Object(record.metadata);
        }
        if content.len() >= 256 {
            if let Some(reference) = self.intern(false, content.into(), offset) {
                stored["payload_ref"] = reference.into();
            } else {
                stored["payload"] = Value::Null;
            }
        } else {
            stored["payload"] = Value::Null;
        }
        // 已知编码长度可直接预留；否则 payload 后的少量字段会令大 Vec 再翻倍。
        // 引用已替换的正文/元数据不应继续计入输出容量。
        let capacity = line
            .len()
            .saturating_sub(if stored.get("meta_ref").is_some() {
                metadata_len
            } else {
                0
            })
            .saturating_sub(if stored.get("payload_ref").is_some() {
                content.len()
            } else {
                0
            })
            .saturating_add(128);
        let mut bytes = Vec::with_capacity(capacity);
        serde_json::to_writer(
            &mut bytes,
            &StoredRecord {
                fields: stored.as_object().ok_or_else(invalid)?,
                payload: record.payload,
            },
        )
        .map_err(io::Error::other)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn intern(&mut self, metadata: bool, bytes: Cow<'_, [u8]>, offset: u64) -> Option<u64> {
        // 超过总预算的记录不可能存在于缓存中，无需复制或计算缓存摘要。
        if bytes.len() > CACHE_BYTES {
            return None;
        }
        let hash: [u8; 32] = Sha256::digest(bytes.as_ref()).into();
        let cache = if metadata {
            &mut self.metadata
        } else {
            &mut self.payloads
        };
        if let Some((previous, reference)) = cache.get(&hash)
            && previous.as_slice() == bytes.as_ref()
        {
            return Some(*reference);
        }
        // 哈希只筛选；碰撞不共享。超大单条内联，不扩大每个活跃 trace 的缓存。
        if self.bytes.saturating_add(bytes.len()) <= CACHE_BYTES && !cache.contains_key(&hash) {
            self.bytes += bytes.len();
            cache.insert(hash, (bytes.into_owned(), offset));
            // Payload lengths already computed for the cache budget; excludes
            // hash-table capacity and allocator overhead.
            self.usage.resize(
                (self.metadata.len() + self.payloads.len()) as u64,
                self.bytes as u64,
            );
        }
        None
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid trace content reference",
    )
}

fn field(
    record: &mut Value,
    name: &str,
    reference_name: &str,
    file: &mut File,
    offset: u64,
) -> io::Result<Value> {
    let object = record.as_object_mut().ok_or_else(invalid)?;
    match (object.remove(name), object.remove(reference_name)) {
        (Some(value), None) => Ok(value),
        (None, Some(reference)) => {
            let position = reference.as_u64().ok_or_else(invalid)?;
            if position >= offset {
                return Err(invalid());
            }
            file.seek(SeekFrom::Start(position))?;
            let mut line = Vec::new();
            io::BufReader::new(&mut *file).read_until(b'\n', &mut line)?;
            let mut target: Value = serde_json::from_slice(&line).map_err(io::Error::other)?;
            if target["trace_storage"].as_u64() != Some(1) {
                return Err(invalid());
            }
            // 引用只能指向内联定义，不允许递归链、循环或跨分段查找。
            target
                .as_object_mut()
                .and_then(|object| object.remove(name))
                .ok_or_else(invalid)
        }
        _ => Err(invalid()),
    }
}

pub(super) fn decode(line: &[u8], file: &mut File, offset: u64) -> io::Result<Value> {
    let mut stored: Value = serde_json::from_slice(line).map_err(io::Error::other)?;
    if stored["trace_storage"].as_u64() != Some(1) {
        return Err(invalid());
    }
    let mut record = field(&mut stored, "meta", "meta_ref", file, offset)?;
    let object = record.as_object_mut().ok_or_else(invalid)?;
    if object.contains_key("payload")
        || object.contains_key("sequence")
        || object.contains_key("recorded_at")
    {
        return Err(invalid());
    }
    let stored_object = stored.as_object_mut().ok_or_else(invalid)?;
    object.insert(
        "sequence".into(),
        stored_object.remove("sequence").ok_or_else(invalid)?,
    );
    object.insert(
        "recorded_at".into(),
        stored_object.remove("recorded_at").ok_or_else(invalid)?,
    );
    object.insert(
        "payload".into(),
        field(&mut stored, "payload", "payload_ref", file, offset)?,
    );
    Ok(record)
}

pub(super) fn visit(
    path: &Path,
    bytes: u64,
    mut visitor: impl FnMut(Value) -> io::Result<()>,
) -> io::Result<()> {
    let mut reader = io::BufReader::new(File::open(path)?.take(bytes));
    let mut references = File::open(path)?;
    let mut offset = 0;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            return Err(invalid());
        }
        visitor(decode(&line, &mut references, offset)?)?;
        offset += read as u64;
    }
    Ok(())
}

/// 仅用于独占的离线副本；完整语义核验通过才替换单个分段。
pub(super) fn optimize_segment(path: &Path) -> io::Result<u64> {
    let original_bytes = path.metadata()?.len();
    let mut output = tempfile::NamedTempFile::new_in(path.parent().ok_or_else(invalid)?)?;
    let mut encoder = Encoder::default();
    let mut offset = 0;
    visit(path, original_bytes, |record| {
        let line = serde_json::to_vec(&record).map_err(io::Error::other)?;
        let bytes = encoder.encode(&line, offset)?;
        output.write_all(&bytes)?;
        offset += bytes.len() as u64;
        Ok(())
    })?;
    output.as_file().sync_all()?;
    let mut encoded = io::BufReader::new(File::open(output.path())?);
    let mut references = File::open(output.path())?;
    let mut position = 0;
    visit(path, original_bytes, |original| {
        let mut line = Vec::new();
        let read = encoded.read_until(b'\n', &mut line)?;
        if decode(&line, &mut references, position)? != original {
            return Err(invalid());
        }
        position += read as u64;
        Ok(())
    })?;
    if position != offset {
        return Err(invalid());
    }
    drop(encoded);
    drop(references);
    if offset < original_bytes {
        output.persist(path).map_err(|error| error.error)?;
        Ok(offset)
    } else {
        Ok(original_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_payload_preserves_storage_bytes_and_reference_threshold() -> io::Result<()> {
        for payload in [
            serde_json::json!("x".repeat(253)),
            serde_json::json!("x".repeat(254)),
            serde_json::json!({"text": "雪😀\n\\\"\0".repeat(100), "empty": null}),
        ] {
            let metadata = serde_json::json!({
                "layer": "wire",
                "unknown": {"escaped": "\\\"\n", "empty": null},
            });
            let mut record = metadata.clone();
            record["sequence"] = 1.into();
            record["recorded_at"] = 11.into();
            record["payload"] = payload.clone();
            let first = serde_json::json!({
                "trace_storage": 1, "sequence": 1, "recorded_at": 11,
                "meta": metadata, "payload": payload,
            });
            let mut expected = serde_json::to_vec(&first)?;
            expected.push(b'\n');
            let mut encoder = Encoder::default();
            let encoded = encoder.encode(&serde_json::to_vec(&record)?, 0)?;
            assert_eq!(encoded, expected);

            record["sequence"] = 2.into();
            record["recorded_at"] = 22.into();
            let mut second = serde_json::json!({
                "trace_storage": 1, "sequence": 2, "recorded_at": 22, "meta_ref": 0,
            });
            if serde_json::to_vec(&payload)?.len() >= 256 {
                second["payload_ref"] = 0.into();
            } else {
                second["payload"] = payload;
            }
            let mut expected = serde_json::to_vec(&second)?;
            expected.push(b'\n');
            assert_eq!(
                encoder.encode(&serde_json::to_vec(&record)?, encoded.len() as u64)?,
                expected,
            );
        }
        Ok(())
    }

    #[test]
    fn cache_reset_and_oversized_inline_records_keep_references_valid() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("segment.jsonl");
        let small = serde_json::json!({
            "sequence": 1, "recorded_at": 1, "layer": "wire",
            "payload": "repeat".repeat(100)
        });
        let oversized = serde_json::json!({
            "sequence": 2, "recorded_at": 2, "layer": "wire",
            "payload": "x".repeat(CACHE_BYTES + 1)
        });
        let records = [&small, &small, &oversized, &small, &small];
        let mut encoder = Encoder::default();
        let mut file = File::create(&path)?;
        let mut offset = 0;
        for record in records {
            let encoded = encoder.encode(&serde_json::to_vec(record)?, offset)?;
            file.write_all(&encoded)?;
            offset += encoded.len() as u64;
        }
        drop(file);
        let mut restored = Vec::new();
        visit(&path, offset, |record| {
            restored.push(record);
            Ok(())
        })?;
        assert_eq!(restored, records.into_iter().cloned().collect::<Vec<_>>());
        Ok(())
    }

    #[test]
    fn encoded_segment_round_trips_and_legacy_records_are_rejected() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("segment-000001.jsonl");
        let payload = serde_json::json!({"text":"保留全部内容\n".repeat(2000)});
        let records = (0..5)
            .map(|sequence| {
                serde_json::json!({
                    "sequence":sequence,"recorded_at":sequence*11,"layer":"canonical",
                    "stage":"stage","payload":payload,"unknown":{"keep":true}
                })
            })
            .collect::<Vec<_>>();
        let mut encoder = Encoder::default();
        let mut file = File::create(&path)?;
        let mut offset = 0;
        for record in &records {
            let bytes = encoder.encode(&serde_json::to_vec(record)?, offset)?;
            offset += bytes.len() as u64;
            file.write_all(&bytes)?;
        }
        file.sync_all()?;
        drop(file);
        let mut restored = Vec::new();
        visit(&path, offset, |record| {
            restored.push(record);
            Ok(())
        })?;
        assert_eq!(restored, records);
        // 当前格式已是编码不动点；优化只校验还原并可能收缩，绝不放大。
        assert_eq!(optimize_segment(&path)?, offset);
        let corrupt = serde_json::json!({
            "trace_storage":1,"sequence":10,"recorded_at":10,"meta_ref":offset+1,"payload":null
        });
        assert!(
            decode(
                &serde_json::to_vec(&corrupt)?,
                &mut File::open(&path)?,
                offset
            )
            .is_err()
        );
        // 无 trace_storage 标记的旧版裸记录不再透传解码。
        assert!(
            decode(
                &serde_json::to_vec(&records[0])?,
                &mut File::open(&path)?,
                0
            )
            .is_err()
        );
        Ok(())
    }
}
