//! 一轮 Debug 会话的历史溢写。匿名文件最后一个句柄关闭即由系统删除。
//! 业务线程只尝试入有界队列；关闭会话撤销旧队列，所有文件 IO 均在专用线程。
use super::*;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};

// 单位是整批；每批分别受 Timeline 的操作、SQL 或采样容量约束。
pub(super) const QUEUE_CAPACITY: usize = 4;

pub(super) enum Pending {
    Traces(VecDeque<TraceRecord>),
    Samples(VecDeque<ResourceSample>),
}
impl Pending {
    fn size(&self) -> (usize, u64) {
        match self {
            Self::Traces(records) => (
                records
                    .front()
                    .map_or(0, |record| usize::from(record.kind != SpanKind::Operation)),
                records.len() as u64,
            ),
            Self::Samples(samples) => (2, samples.len() as u64),
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    sequence: u64,
    kind: usize,
    events: Vec<Value>,
}
#[derive(Default)]
struct Counters {
    dropped: [AtomicU64; 3],
    errors: AtomicU64,
}
enum Command {
    Append {
        generation: u64,
        sequence: u64,
        pending: Pending,
        counters: Arc<Counters>,
    },
    Snapshot {
        generation: u64,
        through_sequence: u64,
        counters: Arc<Counters>,
        reply: mpsc::Sender<Snapshot>,
    },
    Reset,
}
pub(super) struct History {
    sender: Option<SyncSender<Command>>,
    generation: Arc<AtomicU64>,
    counters: Arc<Counters>,
    sequence: u64,
}
pub(super) struct Barrier {
    sender: Option<SyncSender<Command>>,
    generation: Arc<AtomicU64>,
    captured_generation: u64,
    through_sequence: u64,
    counters: Arc<Counters>,
}
pub(super) struct Snapshot {
    pub events: Vec<Value>,
    pub samples: usize,
    pub dropped: [u64; 3],
    pub spilled: [u64; 3],
    pub errors: u64,
    pub status: &'static str,
    pub session_ended: bool,
}
impl Snapshot {
    fn new(counters: &Counters) -> Self {
        let errors = counters.errors.load(Ordering::Relaxed);
        Self {
            events: Vec::new(),
            samples: 0,
            dropped: std::array::from_fn(|i| counters.dropped[i].load(Ordering::Relaxed)),
            spilled: [0; 3],
            errors,
            status: if errors == 0 { "ready" } else { "error" },
            session_ended: false,
        }
    }
}
impl History {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = generation.clone();
        let counters = Arc::new(Counters::default());
        let sender = match std::thread::Builder::new()
            .name("performance-history".into())
            .spawn(move || {
                Worker::new(worker_generation).run(receiver);
            }) {
            Ok(_) => Some(sender),
            Err(_) => {
                counters.errors.fetch_add(1, Ordering::Relaxed);
                None
            }
        };
        Self {
            sender,
            generation,
            counters,
            sequence: 0,
        }
    }
    pub fn reset(&mut self) {
        // 不把 Reset 排在历史队列末尾等待：代号立即撤销队列内的旧记录。
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.counters = Arc::new(Counters::default());
        self.sequence = 0;
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(Command::Reset);
        }
    }
    pub fn push(&mut self, pending: Pending) {
        let (kind, count) = pending.size();
        self.sequence += 1;
        let command = Command::Append {
            generation: self.generation.load(Ordering::Acquire),
            sequence: self.sequence,
            pending,
            counters: self.counters.clone(),
        };
        let result = self.sender.as_ref().map(|sender| sender.try_send(command));
        match result {
            Some(Ok(())) => (),
            Some(Err(TrySendError::Full(_))) => {
                self.counters.dropped[kind].fetch_add(count, Ordering::Relaxed);
            }
            _ => {
                self.counters.dropped[kind].fetch_add(count, Ordering::Relaxed);
                self.counters.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    pub fn barrier(&self) -> Barrier {
        Barrier {
            sender: self.sender.clone(),
            generation: self.generation.clone(),
            captured_generation: self.generation.load(Ordering::Acquire),
            through_sequence: self.sequence,
            counters: self.counters.clone(),
        }
    }
}
impl Barrier {
    pub fn wait(self) -> Snapshot {
        let (reply, receiver) = mpsc::channel();
        let command = Command::Snapshot {
            generation: self.captured_generation,
            through_sequence: self.through_sequence,
            counters: self.counters.clone(),
            reply,
        };
        let mut snapshot = if self.sender.is_some_and(|queue| queue.send(command).is_ok()) {
            receiver.recv().ok()
        } else {
            None
        }
        .unwrap_or_else(|| {
            self.counters.errors.fetch_add(1, Ordering::Relaxed);
            let mut snapshot = Snapshot::new(&self.counters);
            snapshot.status = "unavailable";
            snapshot
        });
        if self.generation.load(Ordering::Acquire) != self.captured_generation {
            snapshot.events.clear();
            snapshot.session_ended = true;
        }
        snapshot
    }
}
struct Storage {
    file: File,
    end: u64,
    written: [u64; 3],
}
impl Storage {
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            file: tempfile::tempfile()?,
            end: 0,
            written: [0; 3],
        })
    }
    fn append(&mut self, record: &Stored) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(record)?;
        self.file.seek(SeekFrom::Start(self.end))?;
        // 部分写失败回滚长度；后续成功写不能把破损帧混入导出。
        let result = self
            .file
            .write_all(&(bytes.len() as u64).to_le_bytes())
            .and_then(|_| self.file.write_all(&bytes));
        if let Err(error) = result {
            if let Err(rollback_error) = self.file.set_len(self.end) {
                tracing::warn!(%rollback_error, "performance history partial write rollback failed");
            }
            return Err(error);
        }
        self.end += 8 + bytes.len() as u64;
        self.written[record.kind] += 1;
        Ok(())
    }
    fn read(&mut self, position: u64) -> std::io::Result<(Stored, u64)> {
        self.file.seek(SeekFrom::Start(position))?;
        let mut size = [0; 8];
        self.file.read_exact(&mut size)?;
        let size = u64::from_le_bytes(size);
        if size > self.end.saturating_sub(position + 8) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "性能历史帧长度损坏",
            ));
        }
        let mut bytes = vec![0; usize::try_from(size).map_err(std::io::Error::other)?];
        self.file.read_exact(&mut bytes)?;
        Ok((serde_json::from_slice(&bytes)?, position + 8 + size))
    }
}
struct Worker {
    storage: Option<Storage>,
    generation: Arc<AtomicU64>,
    current_generation: u64,
}
impl Worker {
    fn new(generation: Arc<AtomicU64>) -> Self {
        Self {
            storage: None,
            generation,
            current_generation: 0,
        }
    }
    fn sync_generation(&mut self) {
        let generation = self.generation.load(Ordering::Acquire);
        if self.current_generation != generation {
            self.storage = None;
            self.current_generation = generation;
        }
    }
    fn run(mut self, receiver: Receiver<Command>) {
        while let Ok(command) = receiver.recv() {
            self.sync_generation();
            match command {
                Command::Append {
                    generation,
                    sequence,
                    pending,
                    counters,
                } if generation == self.current_generation => {
                    match pending {
                        Pending::Traces(records) => self.append_records(
                            generation,
                            sequence,
                            records.into_iter().map(|record| Stored {
                                sequence: 0,
                                kind: usize::from(record.kind != SpanKind::Operation),
                                events: vec![trace_event(record, "finished")],
                            }),
                            &counters,
                        ),
                        Pending::Samples(samples) => self.append_records(
                            generation,
                            sequence,
                            samples.into_iter().map(|sample| Stored {
                                sequence: 0,
                                kind: 2,
                                events: sample_events(sample),
                            }),
                            &counters,
                        ),
                    }
                    self.sync_generation();
                }
                Command::Snapshot {
                    generation,
                    through_sequence,
                    counters,
                    reply,
                } => {
                    let mut snapshot = Snapshot::new(&counters);
                    if generation == self.current_generation {
                        self.collect(&mut snapshot, &counters, through_sequence);
                    } else {
                        snapshot.session_ended = true;
                    }
                    snapshot.errors = counters.errors.load(Ordering::Relaxed);
                    snapshot.status = if snapshot.errors == 0 {
                        "ready"
                    } else {
                        "error"
                    };
                    let _ = reply.send(snapshot);
                    self.sync_generation();
                }
                _ => (),
            }
        }
    }
    fn append_records(
        &mut self,
        generation: u64,
        sequence: u64,
        records: impl Iterator<Item = Stored>,
        counters: &Counters,
    ) {
        for mut record in records {
            if generation != self.generation.load(Ordering::Acquire) {
                break;
            }
            record.sequence = sequence;
            let result = match &mut self.storage {
                Some(storage) => storage.append(&record),
                None => {
                    Storage::new().and_then(|storage| self.storage.insert(storage).append(&record))
                }
            };
            if result.is_err() {
                counters.errors.fetch_add(1, Ordering::Relaxed);
                counters.dropped[record.kind].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    fn collect(&mut self, snapshot: &mut Snapshot, counters: &Counters, through_sequence: u64) {
        let Some(storage) = &mut self.storage else {
            return;
        };
        // 成功落盘计数不因后续读失败而被伪装成较小值。
        snapshot.spilled = storage.written;
        let mut position = 0;
        while position < storage.end {
            if self.generation.load(Ordering::Acquire) != self.current_generation {
                snapshot.events.clear();
                snapshot.session_ended = true;
                return;
            }
            match storage.read(position) {
                Ok((record, next)) => {
                    if record.sequence > through_sequence {
                        break;
                    }
                    position = next;
                    snapshot.samples += usize::from(record.kind == 2);
                    snapshot.events.extend(record.events);
                }
                Err(_) => {
                    counters.errors.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> ResourceSample {
        ResourceSample {
            at: Instant::now(),
            epoch: 0,
            rss_bytes: None,
            cpu_percent: None,
            resources: Vec::new(),
            pools: Vec::new(),
        }
    }

    fn sample() -> Pending {
        Pending::Samples(VecDeque::from([sample_record()]))
    }

    fn stored_sample() -> Stored {
        Stored {
            sequence: 0,
            kind: 2,
            events: sample_events(sample_record()),
        }
    }

    #[test]
    fn full_or_disconnected_queue_reports_gaps_without_blocking() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut history = History {
            sender: Some(sender),
            generation: Arc::new(AtomicU64::new(0)),
            counters: Arc::new(Counters::default()),
            sequence: 0,
        };
        history.push(sample());
        history.push(Pending::Samples(VecDeque::from([
            sample_record(),
            sample_record(),
            sample_record(),
        ])));
        assert_eq!(history.counters.dropped[2].load(Ordering::Relaxed), 3);
        assert_eq!(history.counters.errors.load(Ordering::Relaxed), 0);
        drop(receiver);
        history.push(sample());
        assert_eq!(history.counters.dropped[2].load(Ordering::Relaxed), 4);
        let unavailable = history.barrier().wait();
        assert_eq!(unavailable.status, "unavailable");
        assert!(unavailable.errors > 0);
        history.reset();
        assert_eq!(history.counters.dropped[2].load(Ordering::Relaxed), 0);
        assert_eq!(history.counters.errors.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn failed_write_is_not_counted_as_successful_spill() {
        let temporary = tempfile::NamedTempFile::new().unwrap();
        let readonly = File::open(temporary.path()).unwrap();
        let generation = Arc::new(AtomicU64::new(0));
        let mut worker = Worker::new(generation);
        worker.storage = Some(Storage {
            file: readonly,
            end: 0,
            written: [0; 3],
        });
        let counters = Arc::new(Counters::default());
        let (sender, receiver) = mpsc::sync_channel(2);
        let (reply, result) = mpsc::channel();
        sender
            .send(Command::Append {
                generation: 0,
                sequence: 1,
                pending: sample(),
                counters: counters.clone(),
            })
            .ok()
            .unwrap();
        sender
            .send(Command::Snapshot {
                generation: 0,
                through_sequence: 1,
                counters,
                reply,
            })
            .ok()
            .unwrap();
        drop(sender);
        worker.run(receiver);
        let snapshot = result.recv().unwrap();
        assert_eq!(snapshot.status, "error");
        assert_eq!(snapshot.errors, 1);
        assert_eq!(snapshot.dropped, [0, 0, 1]);
        assert_eq!(snapshot.spilled, [0; 3]);
        assert!(snapshot.events.is_empty());
    }

    #[test]
    fn corrupt_read_is_explicit_and_snapshot_does_not_cache_history() {
        let counters = Arc::new(Counters::default());
        let mut worker = Worker::new(Arc::new(AtomicU64::new(0)));
        let mut storage = Storage::new().unwrap();
        let mut record = stored_sample();
        record.sequence = 1;
        storage.append(&record).unwrap();
        worker.storage = Some(storage);
        let mut first = Snapshot::new(&counters);
        worker.collect(&mut first, &counters, 1);
        assert_eq!(first.samples, 1);
        drop(first);
        // 修改底层文件后必须重新读盘，不能复用上次导出的 events。
        worker.storage.as_mut().unwrap().file.set_len(0).unwrap();
        let mut broken = Snapshot::new(&counters);
        worker.collect(&mut broken, &counters, 1);
        assert!(broken.events.is_empty());
        assert_eq!(counters.errors.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn snapshot_watermark_prevents_duplicate_memory_and_disk_records() {
        let counters = Counters::default();
        let mut worker = Worker::new(Arc::new(AtomicU64::new(0)));
        let mut storage = Storage::new().unwrap();
        for sequence in 1..=2 {
            let mut record = stored_sample();
            record.sequence = sequence;
            storage.append(&record).unwrap();
        }
        worker.storage = Some(storage);
        let mut snapshot = Snapshot::new(&counters);
        worker.collect(&mut snapshot, &counters, 1);
        assert_eq!(snapshot.samples, 1);
        assert_eq!(snapshot.spilled, [0, 0, 2]);
    }

    #[test]
    fn reset_closes_file_and_rejects_pending_previous_session_commands() {
        let generation = Arc::new(AtomicU64::new(0));
        let counters = Arc::new(Counters::default());
        let mut worker = Worker::new(generation.clone());
        worker.storage = Some(Storage::new().unwrap());
        generation.fetch_add(1, Ordering::AcqRel);
        worker.sync_generation();
        assert!(worker.storage.is_none());
        let (sender, receiver) = mpsc::sync_channel(2);
        let (reply, result) = mpsc::channel();
        sender
            .send(Command::Append {
                generation: 0,
                sequence: 1,
                pending: sample(),
                counters: counters.clone(),
            })
            .ok()
            .unwrap();
        sender
            .send(Command::Snapshot {
                generation: 1,
                through_sequence: 0,
                counters,
                reply,
            })
            .ok()
            .unwrap();
        drop(sender);
        worker.run(receiver);
        let snapshot = result.recv().unwrap();
        assert!(snapshot.events.is_empty());
        assert_eq!(snapshot.spilled, [0; 3]);
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn closing_and_reopening_invalidates_in_flight_export_barrier() {
        let mut history = History::new();
        history.push(sample());
        let previous_export = history.barrier();
        history.reset();
        history.reset();
        history.push(sample());
        let previous = previous_export.wait();
        assert!(previous.session_ended);
        assert!(previous.events.is_empty());
        let current = history.barrier().wait();
        assert!(!current.session_ended);
        assert_eq!(current.samples, 1);
        assert_eq!(current.spilled, [0, 0, 1]);
        assert_eq!(current.dropped, [0; 3]);
        assert_eq!(current.errors, 0);
    }
}
