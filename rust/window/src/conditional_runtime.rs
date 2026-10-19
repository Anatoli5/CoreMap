// Один вычислительный поток: неизменяемый снимок сырых журналов, блоки последовательно, запросы вне потока окна.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;

use crate::conditional::{CurveEstimate, CurveLoad};
use crate::conditional_live::{self, Coverage, Options};
use crate::model::{Die, Log};

#[derive(Debug)]
pub struct LiveBlock {
    pub name: String,
    pub observed: Option<CurveEstimate>,
    pub sustained: Option<CurveEstimate>,
    pub status: String,
    pub prepared_rows: usize,
    pub training_rows: usize,
    pub eligible_rows: usize,
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
    pub coverage: Vec<Coverage>,
    pub omitted_contexts: Vec<String>,
    pub options: Options,
}

#[derive(Clone, Debug, Default)]
pub struct LiveCurves {
    pub generation: u64,
    pub source_revision: u64,
    pub pending: bool,
    // Повторная сводка разделяет готовые кривые, не копирует их точки и тем более исходные журналы.
    pub blocks: Vec<Arc<LiveBlock>>,
}

#[derive(Debug)]
pub struct Batch {
    pub generation: u64,
    pub source_revision: u64,
    pub logs: Vec<Log>,
    pub die: Arc<Die>,
    pub options: Options,
}

#[derive(Debug)]
pub struct Runtime {
    work: mpsc::SyncSender<Batch>,
    updates: mpsc::Receiver<Arc<LiveCurves>>,
    generation: Arc<AtomicU64>,
    busy: bool,
}

impl Runtime {
    pub fn start() -> std::io::Result<Self> {
        let (work, incoming) = mpsc::sync_channel::<Batch>(1);
        let (outgoing, updates) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let current = Arc::clone(&generation);
        thread::Builder::new().name("условная модель".into()).spawn(move || {
            while let Ok(batch) = incoming.recv() {
                fit_batch(&batch, &current, &outgoing);
            }
        })?;
        Ok(Self {
            work,
            updates,
            generation,
            busy: false,
        })
    }

    pub fn invalidate(&self, generation: u64) {
        self.generation.store(generation, Ordering::Relaxed);
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn submit(&mut self, batch: Batch) -> Result<(), mpsc::SendError<Batch>> {
        self.generation.store(batch.generation, Ordering::Relaxed);
        self.work.send(batch)?;
        self.busy = true;
        Ok(())
    }

    pub fn poll(&mut self) -> Result<Option<Arc<LiveCurves>>, &'static str> {
        let mut last = None;
        loop {
            match self.updates.try_recv() {
                Ok(update) => {
                    if !update.pending {
                        self.busy = false;
                    }
                    last = Some(update);
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(last),
                Err(mpsc::TryRecvError::Disconnected) => return Err("вычислительный поток завершился"),
            }
        }
    }
}

fn query_pair(fit: &conditional_live::BlockFit) -> (Option<CurveEstimate>, Option<CurveEstimate>, Option<String>) {
    let Some(model) = &fit.model else {
        return (None, None, None);
    };
    match (
        model.curve_query(CurveLoad::ObservedHistory),
        model.curve_query(CurveLoad::SustainedFull),
    ) {
        (Ok(observed), Ok(sustained)) => (Some(observed), Some(sustained), None),
        _ => (
            None,
            None,
            Some("Не удалось построить условный срез модели; прежняя кривая не подставляется".into()),
        ),
    }
}

fn block(fit: conditional_live::BlockFit) -> LiveBlock {
    let (observed, sustained, status) = query_pair(&fit);
    LiveBlock {
        name: fit.name,
        observed,
        sustained,
        status: status.unwrap_or(fit.status),
        prepared_rows: fit.prepared_rows,
        training_rows: fit.training_rows,
        eligible_rows: fit.eligible_rows,
        first_ms: fit.first_ms,
        last_ms: fit.last_ms,
        coverage: fit.coverage,
        omitted_contexts: fit.omitted_contexts,
        options: fit.options,
    }
}

fn fit_batch(batch: &Batch, current: &AtomicU64, outgoing: &mpsc::Sender<Arc<LiveCurves>>) {
    let logs: Vec<&Log> = batch.logs.iter().collect();
    let mut result = LiveCurves {
        generation: batch.generation,
        source_revision: batch.source_revision,
        pending: true,
        blocks: Vec::with_capacity(batch.die.names.len()),
    };
    for b in 0..batch.die.names.len() {
        if current.load(Ordering::Relaxed) != batch.generation {
            break;
        }
        let fit = conditional_live::fit_block(&logs, &batch.die, b, batch.options);
        result.blocks.push(Arc::new(block(fit)));
        // Снимки разделяют лишь небольшие готовые кривые. Матрица текущего блока освобождается перед следующим.
        if outgoing.send(Arc::new(result.clone())).is_err() {
            return; // Окно и читатель уже закрылись, некому показывать результат.
        }
    }
    result.pending = false;
    let _ = outgoing.send(Arc::new(result)); // Закрывшийся читатель больше не ждёт завершения.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch() -> Batch {
        Batch {
            generation: 4,
            source_revision: 9,
            logs: vec![Log::new(vec!["0".into(), "1".into()])],
            die: Arc::new(Die {
                names: vec!["0".into(), "1".into()],
                e: vec![false; 2],
                neighbours: vec![vec![1], vec![0]],
            }),
            options: Options::default(),
        }
    }

    #[test]
    fn failed_block_has_a_reason_and_does_not_stop_other_blocks() {
        let batch = batch();
        let (send, receive) = mpsc::channel();
        fit_batch(&batch, &AtomicU64::new(batch.generation), &send);
        drop(send);
        let updates: Vec<_> = receive.try_iter().collect();
        assert_eq!(updates.len(), 3, "два последовательных блока и завершение");
        assert_eq!(updates[0].blocks.len(), 1);
        assert_eq!(updates[1].blocks.len(), 2);
        let done = &updates[2];
        assert!(!done.pending);
        assert_eq!((done.generation, done.source_revision), (4, 9));
        for (i, block) in done.blocks.iter().enumerate() {
            assert_eq!(block.name, batch.die.names[i]);
            assert!(block.observed.is_none() && block.sustained.is_none());
            assert_ne!(block.status.trim(), "");
        }
    }

    #[test]
    fn replaced_die_generation_cancels_the_old_batch_before_computing_a_block() {
        let batch = batch();
        let (send, receive) = mpsc::channel();
        fit_batch(&batch, &AtomicU64::new(batch.generation + 1), &send);
        drop(send);
        let updates: Vec<_> = receive.try_iter().collect();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].blocks.len(), 0);
        assert!(
            !updates[0].pending,
            "читатель должен узнать, что старый пакет больше не занимает поток"
        );
        assert_eq!(
            updates[0].generation, 4,
            "отменённое поколение не превращается в текущее"
        );
    }
}
