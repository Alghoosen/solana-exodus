#![feature(map_first_last)]

use {
    crossbeam_channel::{bounded, unbounded},
    log::*,
    rand::Rng,
    sha2::{Digest, Sha256},
    solana_measure::measure::Measure,
    solana_metrics::datapoint_info,
    solana_sdk::{
        hash::Hash,
        pubkey::Pubkey,
        transaction::{SanitizedTransaction, TransactionAccountLocks, VersionedTransaction},
    },
};

/*
type PageRcInner<T> = std::rc::Rc<T>;
unsafe impl Send for PageRc {}
*/

type PageRcInner = triomphe::Arc<(std::cell::RefCell<Page>, TaskIds)>;

#[derive(Debug, Clone)]
pub struct PageRc(PageRcInner);
unsafe impl Send for PageRc {}
unsafe impl Sync for PageRc {}

type CU = u64;

#[derive(Debug)]
pub struct ExecutionEnvironment {
    //accounts: Vec<i8>,
    pub cu: CU,
    pub unique_weight: UniqueWeight,
    pub task: TaskInQueue,
    pub finalized_lock_attempts: Vec<LockAttempt>,
    pub is_reindexed: bool,
    pub execution_result: Option<Result<(), solana_sdk::transaction::TransactionError>>,
}

impl ExecutionEnvironment {
    //fn new(cu: usize) -> Self {
    //    Self {
    //        cu,
    //        ..Self::default()
    //    }
    //}

    //fn abort() {
    //  pass AtomicBool into InvokeContext??
    //}
    //
    #[inline(never)]
    fn reindex_with_address_book<AST: AtScheduleThread>(&mut self, ast: AST) {
        assert!(!self.is_reindexed());
        self.is_reindexed = true;

        let uq = self.unique_weight;
        //self.task.trace_timestamps("in_exec(self)");
        let should_remove = self
            .task
            .contention_count
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0;
        for mut lock_attempt in self.finalized_lock_attempts.iter_mut() {
            let contended_unique_weights = lock_attempt.target_contended_unique_weights();
            contended_unique_weights
                .heaviest_task_cursor()
                .map(|mut task_cursor| {
                    let mut found = true;
                    let mut removed = false;
                    let mut task = task_cursor.value();
                    //task.trace_timestamps("in_exec(initial list)");
                    while !task.currently_contended() {
                        if task_cursor.key() == &uq {
                            assert!(should_remove);
                            removed = task_cursor.remove();
                            assert!(removed);
                        }
                        if task.already_finished() {
                            task_cursor.remove();
                        }
                        if let Some(new_cursor) = task_cursor.prev() {
                            assert!(new_cursor.key() < task_cursor.key());
                            task_cursor = new_cursor;
                            task = task_cursor.value();
                            //task.trace_timestamps("in_exec(subsequent list)");
                        } else {
                            found = false;
                            break;
                        }
                    }
                    if should_remove && !removed {
                        contended_unique_weights.remove_task(&uq);
                    }
                    found.then(|| Task::clone_in_queue(task))
                })
                .flatten()
                .map(|task| {
                    //task.trace_timestamps(&format!("in_exec(heaviest:{})", self.task.queue_time_label()));
                    lock_attempt.heaviest_uncontended = Some(task);
                    ()
                });

            if should_remove && lock_attempt.requested_usage == RequestedUsage::Writable {
                let mut page = lock_attempt.target.page_mut(ast);
                page.contended_write_task_count = page.contended_write_task_count.checked_sub(1).unwrap();
            }
        }
    }

    fn is_reindexed(&self) -> bool {
        self.is_reindexed
    }

    pub fn is_aborted(&self) -> bool {
        if let Some(r) = &self.execution_result {
            r.is_err()
        } else {
            false
        }
    }
}

unsafe trait AtScheduleThread: Copy {}
pub unsafe trait NotAtScheduleThread: Copy {}

impl PageRc {
    fn page_mut<AST: AtScheduleThread>(&self, _ast: AST) -> std::cell::RefMut<'_, Page> {
        self.0.0.borrow_mut()
    }
}

#[derive(Clone, Debug)]
enum LockStatus {
    Succeded,
    Provisional,
    Failed,
}

#[derive(Debug)]
pub struct LockAttempt {
    target: PageRc,
    status: LockStatus,
    requested_usage: RequestedUsage,
    //pub heaviest_uncontended: arc_swap::ArcSwapOption<Task>,
    pub heaviest_uncontended: Option<TaskInQueue>,
    //remembered: bool,
}

impl LockAttempt {
    pub fn new(target: PageRc, requested_usage: RequestedUsage) -> Self {
        Self {
            target,
            status: LockStatus::Succeded,
            requested_usage,
            heaviest_uncontended: Default::default(),
            //remembered: false,
        }
    }

    pub fn clone_for_test(&self) -> Self {
        Self {
            target: self.target.clone(),
            status: LockStatus::Succeded,
            requested_usage: self.requested_usage,
            heaviest_uncontended: Default::default(),
            //remembered: false,
        }
    }

    pub fn target_contended_unique_weights(&self) -> &TaskIds {
        &self.target.0.1
    }
}

type UsageCount = usize;
const SOLE_USE_COUNT: UsageCount = 1;

#[derive(Copy, Clone, Debug, PartialEq)]
enum Usage {
    Unused,
    // weight to abort running tx?
    // also sum all readonly weights to subvert to write lock with greater weight?
    Readonly(UsageCount),
    Writable,
}

impl Usage {
    fn renew(requested_usage: RequestedUsage) -> Self {
        match requested_usage {
            RequestedUsage::Readonly => Usage::Readonly(SOLE_USE_COUNT),
            RequestedUsage::Writable => Usage::Writable,
        }
    }

    fn unused() -> Self {
        Usage::Unused
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestedUsage {
    Readonly,
    Writable,
}

#[derive(Debug, Default)]
pub struct TaskIds {
    task_ids: crossbeam_skiplist::SkipMap<UniqueWeight, TaskInQueue>,
}

impl TaskIds {
    #[inline(never)]
    pub fn insert_task(&self, u: TaskId, task: TaskInQueue) {
        let mut is_inserted = false;
        self.task_ids.get_or_insert_with(u, || {
            is_inserted = true;
            task
        });
        assert!(is_inserted);
    }

    #[inline(never)]
    pub fn remove_task(&self, u: &TaskId) {
        let removed_entry = self.task_ids.remove(u);
        assert!(removed_entry.is_some());
    }

    #[inline(never)]
    pub fn heaviest_task_cursor(
        &self,
    ) -> Option<crossbeam_skiplist::map::Entry<'_, UniqueWeight, TaskInQueue>> {
        self.task_ids.back()
    }
}

#[derive(Debug)]
pub struct Page {
    address_str: String,
    current_usage: Usage,
    next_usage: Usage,
    provisional_task_ids: Vec<triomphe::Arc<ProvisioningTracker>>,
    cu: CU,
    contended_write_task_count: usize,
    //loaded account from Accounts db
    //comulative_cu for qos; i.e. track serialized cumulative keyed by addresses and bail out block
    //producing as soon as any one of cu from the executing thread reaches to the limit
}

impl Page {
    fn new(address: &Pubkey, current_usage: Usage) -> Self {
        Self {
            address_str: format!("{}", address),
            current_usage,
            next_usage: Usage::Unused,
            provisional_task_ids: Default::default(),
            cu: Default::default(),
            contended_write_task_count: Default::default(),
        }
    }

    fn switch_to_next_usage(&mut self) {
        self.current_usage = self.next_usage;
        self.next_usage = Usage::Unused;
    }
}

//type AddressMap = std::collections::HashMap<Pubkey, PageRc>;
type AddressMap = std::sync::Arc<dashmap::DashMap<Pubkey, PageRc>>;
type TaskId = UniqueWeight;
type WeightedTaskIds = std::collections::BTreeMap<TaskId, TaskInQueue>;
//type AddressMapEntry<'a, K, V> = std::collections::hash_map::Entry<'a, K, V>;
type AddressMapEntry<'a> = dashmap::mapref::entry::Entry<'a, Pubkey, PageRc>;

type StuckTaskId = (CU, TaskId);

// needs ttl mechanism and prune
#[derive(Default)]
pub struct AddressBook {
    book: AddressMap,
    uncontended_task_ids: WeightedTaskIds,
    fulfilled_provisional_task_ids: WeightedTaskIds,
    stuck_tasks: std::collections::BTreeMap<StuckTaskId, TaskInQueue>,
}

#[derive(Debug)]
struct ProvisioningTracker {
    remaining_count: std::sync::atomic::AtomicUsize,
    task: TaskInQueue,
}

impl ProvisioningTracker {
    fn new(remaining_count: usize, task: TaskInQueue) -> Self {
        Self {
            remaining_count: std::sync::atomic::AtomicUsize::new(remaining_count),
            task,
        }
    }

    fn is_fulfilled(&self) -> bool {
        self.count() == 0
    }

    fn progress(&self) {
        self.remaining_count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn prev_count(&self) -> usize {
        self.count() + 1
    }

    fn count(&self) -> usize {
        self.remaining_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl AddressBook {
    #[inline(never)]
    fn attempt_lock_address<AST: AtScheduleThread>(
        ast: AST,
        from_runnable: bool,
        prefer_immediate: bool,
        unique_weight: &UniqueWeight,
        attempt: &mut LockAttempt,
    ) -> CU {
        let strictly_lockable_for_replay = if attempt.target_contended_unique_weights().task_ids.is_empty() {
            true
        } else if attempt.target_contended_unique_weights().task_ids.back().unwrap().key() == unique_weight {
            true
        } else if attempt.requested_usage == RequestedUsage::Readonly && attempt.target.page_mut(ast).contended_write_task_count == 0 {
            true
        } else {
            false
        };

        if !strictly_lockable_for_replay {
            attempt.status = LockStatus::Failed;
            let page = attempt.target.page_mut(ast);
            return page.cu;
        }


        let LockAttempt {
            target,
            requested_usage,
            status, /*, remembered*/
            ..
        } = attempt;
        let mut page = target.page_mut(ast);


        let next_usage = page.next_usage;
        match page.current_usage {
            Usage::Unused => {
                assert_eq!(page.next_usage, Usage::Unused);
                page.current_usage = Usage::renew(*requested_usage);
                *status = LockStatus::Succeded;
            }
            Usage::Readonly(ref mut count) => match requested_usage {
                RequestedUsage::Readonly => {
                    // prevent newer read-locks (even from runnable too)
                    match next_usage {
                        Usage::Unused => {
                            *count += 1;
                            *status = LockStatus::Succeded;
                        }
                        Usage::Readonly(_) | Usage::Writable => {
                            *status = LockStatus::Failed;
                        }
                    }
                }
                RequestedUsage::Writable => {
                    if from_runnable || prefer_immediate {
                        *status = LockStatus::Failed;
                    } else {
                        match page.next_usage {
                            Usage::Unused => {
                                *status = LockStatus::Provisional;
                                page.next_usage = Usage::renew(*requested_usage);
                            }
                            // support multiple readonly locks!
                            Usage::Readonly(_) | Usage::Writable => {
                                *status = LockStatus::Failed;
                            }
                        }
                    }
                }
            },
            Usage::Writable => {
                if from_runnable || prefer_immediate {
                    *status = LockStatus::Failed;
                } else {
                    match page.next_usage {
                        Usage::Unused => {
                            *status = LockStatus::Provisional;
                            page.next_usage = Usage::renew(*requested_usage);
                        }
                        // support multiple readonly locks!
                        Usage::Readonly(_) | Usage::Writable => {
                            *status = LockStatus::Failed;
                        }
                    }
                }
            }
        }
        page.cu
    }

    fn reset_lock<AST: AtScheduleThread>(
        &mut self,
        ast: AST,
        attempt: &mut LockAttempt,
        after_execution: bool,
    ) -> bool {
        match attempt.status {
            LockStatus::Succeded => self.unlock(ast, attempt),
            LockStatus::Provisional => {
                if after_execution {
                    self.unlock(ast, attempt)
                } else {
                    self.cancel(ast, attempt);
                    false
                }
            }
            LockStatus::Failed => {
                false // do nothing
            }
        }
    }

    #[inline(never)]
    fn unlock<AST: AtScheduleThread>(&mut self, ast: AST, attempt: &mut LockAttempt) -> bool {
        //debug_assert!(attempt.is_success());

        let mut newly_uncontended = false;

        let mut page = attempt.target.page_mut(ast);

        match &mut page.current_usage {
            Usage::Readonly(ref mut count) => match &attempt.requested_usage {
                RequestedUsage::Readonly => {
                    if *count == SOLE_USE_COUNT {
                        newly_uncontended = true;
                    } else {
                        *count -= 1;
                    }
                }
                RequestedUsage::Writable => unreachable!(),
            },
            Usage::Writable => match &attempt.requested_usage {
                RequestedUsage::Writable => {
                    newly_uncontended = true;
                }
                RequestedUsage::Readonly => unreachable!(),
            },
            Usage::Unused => unreachable!(),
        }

        if newly_uncontended {
            page.current_usage = Usage::Unused;
        }

        newly_uncontended
    }

    #[inline(never)]
    fn cancel<AST: AtScheduleThread>(&mut self, ast: AST, attempt: &mut LockAttempt) {
        let mut page = attempt.target.page_mut(ast);

        match page.next_usage {
            Usage::Unused => {
                unreachable!();
            }
            // support multiple readonly locks!
            Usage::Readonly(_) | Usage::Writable => {
                page.next_usage = Usage::Unused;
            }
        }
    }

    pub fn preloader(&self) -> Preloader {
        Preloader {
            book: std::sync::Arc::clone(&self.book),
        }
    }
}

#[derive(Debug)]
pub struct Preloader {
    book: AddressMap,
}

impl Preloader {
    #[inline(never)]
    pub fn load(&self, address: Pubkey) -> PageRc {
        PageRc::clone(&self.book.entry(address).or_insert_with(|| {
            PageRc(PageRcInner::new((
                core::cell::RefCell::new(Page::new(&address, Usage::unused())),
                Default::default(),
            )))
        }))
    }
}

/*
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Weight {
    // naming: Sequence Ordering?
    pub ix: u64, // index in ledger entry?
                   // gas fee
}

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct UniqueWeight {
    // naming: Sequence Ordering?
    weight: Weight,
    // we can't use Transaction::message_hash because it's manipulatable to be favorous to the tx
    // submitter
    //unique_key: Hash, // tie breaker? random noise? also for unique identification of txes?
    unique_key: u64, // tie breaker? random noise? also for unique identification of txes?
    // fee?
}
*/
pub type Weight = u64;
pub type UniqueWeight = u64;

struct Bundle {
    // what about bundle1{tx1a, tx2} and bundle2{tx1b, tx2}?
}

#[derive(Debug)]
pub struct Task {
    unique_weight: UniqueWeight,
    pub tx: (SanitizedTransaction, LockAttemptsInCell), // actually should be Bundle
    pub contention_count: std::sync::atomic::AtomicUsize,
    pub busiest_page_cu: std::sync::atomic::AtomicU64,
    pub uncontended: std::sync::atomic::AtomicUsize,
    pub sequence_time: std::sync::atomic::AtomicUsize,
    pub sequence_end_time: std::sync::atomic::AtomicUsize,
    pub queue_time: std::sync::atomic::AtomicUsize,
    pub queue_end_time: std::sync::atomic::AtomicUsize,
    pub execute_time: std::sync::atomic::AtomicUsize,
    pub commit_time: std::sync::atomic::AtomicUsize,
    pub for_indexer: LockAttemptsInCell,
}

#[derive(Debug)]
pub struct LockAttemptsInCell(std::cell::RefCell<Vec<LockAttempt>>);

unsafe impl Send for LockAttemptsInCell {}
unsafe impl Sync for LockAttemptsInCell {}

impl LockAttemptsInCell {
    fn new(ll: std::cell::RefCell<Vec<LockAttempt>>) -> Self {
        Self(ll)
    }
}

// sequence_time -> seq clock
// queue_time -> queue clock
// execute_time ---> exec clock
// commit_time  -+

impl Task {
    pub fn new_for_queue<NAST: NotAtScheduleThread>(
        nast: NAST,
        unique_weight: UniqueWeight,
        tx: (SanitizedTransaction, Vec<LockAttempt>),
    ) -> TaskInQueue {
        TaskInQueue::new(Self {
            for_indexer: LockAttemptsInCell::new(std::cell::RefCell::new(
                tx.1.iter().map(|a| a.clone_for_test()).collect(),
            )),
            unique_weight,
            tx: (tx.0, LockAttemptsInCell::new(std::cell::RefCell::new(tx.1))),
            contention_count: Default::default(),
            busiest_page_cu: Default::default(),
            uncontended: Default::default(),
            sequence_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
            sequence_end_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
            queue_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
            queue_end_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
            execute_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
            commit_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
        })
    }

    pub fn transaction_index_in_entries_for_replay(&self) -> u64 {
        u64::max_value() - self.unique_weight
    }

    #[inline(never)]
    pub fn clone_in_queue(this: &TaskInQueue) -> TaskInQueue {
        TaskInQueue::clone(this)
    }

    fn lock_attempts_mut<AST: AtScheduleThread>(
        &self,
        _ast: AST,
    ) -> std::cell::RefMut<'_, Vec<LockAttempt>> {
        self.tx.1.0.borrow_mut()
    }

    fn lock_attempts_not_mut<NAST: NotAtScheduleThread>(
        &self,
        _nast: NAST,
    ) -> std::cell::Ref<'_, Vec<LockAttempt>> {
        self.tx.1.0.borrow()
    }

    fn update_busiest_page_cu(&self, cu: CU) {
        self.busiest_page_cu
            .store(cu, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn record_sequence_time(&self, clock: usize) {
        //self.sequence_time.store(clock, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn sequence_time(&self) -> usize {
        self.sequence_time.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn sequence_end_time(&self) -> usize {
        self.sequence_end_time
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn record_queue_time(&self, seq_clock: usize, queue_clock: usize) {
        //self.sequence_end_time.store(seq_clock, std::sync::atomic::Ordering::SeqCst);
        //self.queue_time.store(queue_clock, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn queue_time(&self) -> usize {
        self.queue_time.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn queue_end_time(&self) -> usize {
        self.queue_end_time
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn record_execute_time(&self, queue_clock: usize, execute_clock: usize) {
        //self.queue_end_time.store(queue_clock, std::sync::atomic::Ordering::SeqCst);
        self.execute_time.store(execute_clock, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn execute_time(&self) -> usize {
        self.execute_time.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn record_commit_time(&self, execute_clock: usize) {
        //self.commit_time.store(execute_clock, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn commit_time(&self) -> usize {
        self.commit_time.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn queue_time_label(&self) -> String {
        format!(
            "queue: [{}qT..{}qT; {}qD]",
            self.queue_time(),
            self.queue_end_time(),
            self.queue_end_time() - self.queue_time(),
        )
    }

    pub fn trace_timestamps(&self, prefix: &str) {
        trace!("{}: {:016x} seq: [{}sT..{}sT; {}sD], queue: [{}qT..{}qT; {}qD] exec: [{}eT..{}eT; {}eD]", 
              prefix,
              self.unique_weight,
              self.sequence_time(),
              self.sequence_end_time(),
              self.sequence_end_time() - self.sequence_time(),
              self.queue_time(), self.queue_end_time(), self.queue_end_time() - self.queue_time(),
              self.execute_time(), self.commit_time(), self.commit_time() - self.execute_time());
    }

    pub fn clone_for_test<NAST: NotAtScheduleThread>(&self, nast: NAST) -> Self {
        Self {
            unique_weight: self.unique_weight,
            for_indexer: LockAttemptsInCell::new(std::cell::RefCell::new(
                self.lock_attempts_not_mut(nast)
                    .iter()
                    .map(|a| a.clone_for_test())
                    .collect(),
            )),
            tx: (
                self.tx.0.clone(),
                LockAttemptsInCell::new(std::cell::RefCell::new(
                    self.lock_attempts_not_mut(nast)
                        .iter()
                        .map(|l| l.clone_for_test())
                        .collect::<Vec<_>>(),
                )),
            ),
            contention_count: Default::default(),
            busiest_page_cu: Default::default(),
            uncontended: Default::default(),
            sequence_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
            sequence_end_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
            queue_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
            queue_end_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
            execute_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
            commit_time: std::sync::atomic::AtomicUsize::new(usize::max_value()),
        }
    }

    pub fn currently_contended(&self) -> bool {
        self.uncontended.load(std::sync::atomic::Ordering::SeqCst) == 1
    }

    pub fn already_finished(&self) -> bool {
        self.uncontended.load(std::sync::atomic::Ordering::SeqCst) == 3
    }

    fn mark_as_contended(&self) {
        self.uncontended
            .store(1, std::sync::atomic::Ordering::SeqCst)
    }

    fn mark_as_uncontended(&self) {
        assert!(self.currently_contended());
        self.uncontended
            .store(2, std::sync::atomic::Ordering::SeqCst)
    }

    fn mark_as_finished(&self) {
        assert!(!self.already_finished() && !self.currently_contended());
        self.uncontended
            .store(3, std::sync::atomic::Ordering::SeqCst)
    }

    #[inline(never)]
    fn index_with_address_book<AST: AtScheduleThread>(
        ast: AST,
        this: &TaskInQueue,
        task_sender: &crossbeam_channel::Sender<(TaskInQueue, Vec<LockAttempt>)>,
    ) {
        for lock_attempt in &*this.lock_attempts_mut(ast) {
            lock_attempt.target_contended_unique_weights().insert_task(this.unique_weight, Task::clone_in_queue(this));

            if lock_attempt.requested_usage == RequestedUsage::Writable {
                let mut page = lock_attempt.target.page_mut(ast);
                page.contended_write_task_count = page.contended_write_task_count.checked_add(1).unwrap();
            }
        }
        //let a = Task::clone_in_queue(this);
        //task_sender
        //    .send((a, std::mem::take(&mut *this.for_indexer.0.borrow_mut())))
        //    .unwrap();
    }

    fn stuck_task_id(&self) -> StuckTaskId {
        let cu = self
            .busiest_page_cu
            .load(std::sync::atomic::Ordering::SeqCst);
        assert_ne!(cu, 0);
        (cu, TaskId::max_value() - self.unique_weight)
    }

    pub fn contention_count(&self) -> usize {
        self.contention_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

// RunnableQueue, ContendedQueue?
#[derive(Default, Debug, Clone)]
pub struct TaskQueue {
    tasks: std::collections::BTreeMap<UniqueWeight, TaskInQueue>,
    //tasks: im::OrdMap<UniqueWeight, TaskInQueue>,
    //tasks: im::HashMap<UniqueWeight, TaskInQueue>,
    //tasks: std::sync::Arc<dashmap::DashMap<UniqueWeight, TaskInQueue>>,
}

pub type TaskInQueue = triomphe::Arc<Task>;

//type TaskQueueEntry<'a> = im::ordmap::Entry<'a, UniqueWeight, TaskInQueue>;
//type TaskQueueOccupiedEntry<'a> = im::ordmap::OccupiedEntry<'a, UniqueWeight, TaskInQueue>;
//type TaskQueueEntry<'a> = im::hashmap::Entry<'a, UniqueWeight, TaskInQueue, std::collections::hash_map::RandomState>;
//type TaskQueueOccupiedEntry<'a> = im::hashmap::OccupiedEntry<'a, UniqueWeight, TaskInQueue, std::collections::hash_map::RandomState>;
//type TaskQueueEntry<'a> = dashmap::mapref::entry::Entry<'a, UniqueWeight, TaskInQueue>;
//type TaskQueueOccupiedEntry<'a> = dashmap::mapref::entry::OccupiedEntry<'a, UniqueWeight, TaskInQueue, std::collections::hash_map::RandomState>;
type TaskQueueEntry<'a> = std::collections::btree_map::Entry<'a, UniqueWeight, TaskInQueue>;
type TaskQueueOccupiedEntry<'a> =
    std::collections::btree_map::OccupiedEntry<'a, UniqueWeight, TaskInQueue>;

impl TaskQueue {
    #[inline(never)]
    fn add_to_schedule(&mut self, unique_weight: UniqueWeight, task: TaskInQueue) {
        //trace!("TaskQueue::add(): {:?}", unique_weight);
        let pre_existed = self.tasks.insert(unique_weight, task);
        assert!(pre_existed.is_none()); //, "identical shouldn't exist: {:?}", unique_weight);
    }

    #[inline(never)]
    fn heaviest_entry_to_execute(&mut self) -> Option<TaskQueueOccupiedEntry<'_>> {
        self.tasks.last_entry()
    }

    fn task_count(&self) -> usize {
        self.tasks.len()
    }
}

#[inline(never)]
fn attempt_lock_for_execution<'a, AST: AtScheduleThread>(
    ast: AST,
    from_runnable: bool,
    prefer_immediate: bool,
    address_book: &mut AddressBook,
    unique_weight: &UniqueWeight,
    message_hash: &'a Hash,
    lock_attempts: &mut [LockAttempt],
) -> (usize, usize, CU) {
    // no short-cuircuit; we at least all need to add to the contended queue
    let mut unlockable_count = 0;
    let mut provisional_count = 0;
    let mut busiest_page_cu = 1;

    for attempt in lock_attempts.iter_mut() {
        let cu = AddressBook::attempt_lock_address(
            ast,
            from_runnable,
            prefer_immediate,
            unique_weight,
            attempt,
        );
        busiest_page_cu = busiest_page_cu.max(cu);

        match attempt.status {
            LockStatus::Succeded => {}
            LockStatus::Failed => {
                trace!("lock failed: {}/{:?}", attempt.target.page_mut(ast).address_str, attempt.requested_usage);
                unlockable_count += 1;
            }
            LockStatus::Provisional => {
                provisional_count += 1;
            }
        }
    }

    (unlockable_count, provisional_count, busiest_page_cu)
}

type PreprocessedTransaction = (SanitizedTransaction, Vec<LockAttempt>);

/*
pub fn get_transaction_priority_details(tx: &SanitizedTransaction) -> u64 {
    use solana_program_runtime::compute_budget::ComputeBudget;
    let mut compute_budget = ComputeBudget::default();
    compute_budget
        .process_instructions(
            tx.message().program_instructions_iter(),
            true, // use default units per instruction
        )
        .map(|d| d.get_priority())
        .unwrap_or_default()
}
*/

pub struct ScheduleStage {}

#[derive(PartialEq, Eq)]
enum TaskSource {
    Runnable,
    Contended,
    Stuck,
}

/*
enum SelectionContext {
    Runnable,
    Contended(u8, Vec<TaskInQueue>),
}

impl SelectionContext {
    fn should_continue(&self) -> bool {
        match(self) {
            SelectionContext::Runnable => true,
            SelectionContext::Contended(failure_count) => failure_count < 2,
        }
    }

    fn runnable_exclusive(&self) -> bool {
        match(self) {
            SelectionContext::Runnable => true,
            SelectionContext::Contended(_) => false,
        }
    }
}
*/

impl ScheduleStage {
    fn push_to_runnable_queue(task: TaskInQueue, runnable_queue: &mut TaskQueue) {
        runnable_queue.add_to_schedule(task.unique_weight, task);
    }

    #[inline(never)]
    fn get_heaviest_from_contended<'a>(
        address_book: &'a mut AddressBook,
    ) -> Option<std::collections::btree_map::OccupiedEntry<'a, UniqueWeight, TaskInQueue>> {
        address_book.uncontended_task_ids.last_entry()
    }

    #[inline(never)]
    fn select_next_task<'a>(
        runnable_queue: &'a mut TaskQueue,
        address_book: &mut AddressBook,
        contended_count: &usize,
        runnable_exclusive: bool,
    ) -> Option<(TaskSource, TaskInQueue)> {
        match (
            runnable_queue.heaviest_entry_to_execute(),
            Self::get_heaviest_from_contended(address_book),
        ) {
            (Some(heaviest_runnable_entry), None) => {
                trace!("select: runnable only");
                if runnable_exclusive {
                    let t = heaviest_runnable_entry.remove();
                    Some((TaskSource::Runnable, t))
                } else {
                    None
                }
            }
            (None, Some(weight_from_contended)) => {
                trace!("select: contended only");
                if runnable_exclusive {
                    None
                } else {
                    let t = weight_from_contended.remove();
                    Some((TaskSource::Contended, t))
                }
            }
            (Some(heaviest_runnable_entry), Some(weight_from_contended)) => {
                let weight_from_runnable = heaviest_runnable_entry.key();
                let uw = weight_from_contended.key();

                if weight_from_runnable > uw {
                    panic!("replay shouldn't see this branch: {} > {}", weight_from_runnable, uw);

                    /*
                    trace!("select: runnable > contended");
                    let t = heaviest_runnable_entry.remove();
                    Some((TaskSource::Runnable, t))
                    */
                } else if uw > weight_from_runnable {
                    if runnable_exclusive {
                        trace!("select: contended > runnnable, runnable_exclusive");
                        let t = heaviest_runnable_entry.remove();
                        Some((TaskSource::Runnable, t))
                    } else {
                        trace!("select: contended > runnnable, !runnable_exclusive)");
                        let t = weight_from_contended.remove();
                        Some((TaskSource::Contended, t))
                    }
                } else {
                    unreachable!(
                        "identical unique weights shouldn't exist in both runnable and contended"
                    )
                }
            }
            (None, None) => {
                trace!("select: none");

                if false && runnable_queue.task_count() == 0 && /* *contended_count > 0 &&*/ address_book.stuck_tasks.len() > 0
                {
                    trace!("handling stuck...");
                    let (stuck_task_id, task) = address_book.stuck_tasks.pop_first().unwrap();
                    // ensure proper rekeying
                    assert_eq!(task.stuck_task_id(), stuck_task_id);

                    if task.currently_contended() {
                        Some((TaskSource::Stuck, task))
                    } else {
                        // is it expected for uncontended tasks is in the stuck queue, to begin
                        // with??
                        None
                    }
                } else {
                    None
                }
            }
        }
    }

    #[inline(never)]
    fn pop_from_queue_then_lock<AST: AtScheduleThread>(
        ast: AST,
        task_sender: &crossbeam_channel::Sender<(TaskInQueue, Vec<LockAttempt>)>,
        runnable_queue: &mut TaskQueue,
        address_book: &mut AddressBook,
        contended_count: &mut usize,
        prefer_immediate: bool,
        sequence_clock: &usize,
        queue_clock: &mut usize,
        provisioning_tracker_count: &mut usize,
        runnable_exclusive: bool,
    ) -> Option<(UniqueWeight, TaskInQueue, Vec<LockAttempt>)> {
        if let Some(mut a) = address_book.fulfilled_provisional_task_ids.pop_last() {
            trace!(
                "expediate pop from provisional queue [rest: {}]",
                address_book.fulfilled_provisional_task_ids.len()
            );

            let lock_attempts = std::mem::take(&mut *a.1.lock_attempts_mut(ast));

            return Some((a.0, a.1, lock_attempts));
        }

        trace!("pop begin");
        loop {
            if let Some((task_source, mut next_task)) =
                Self::select_next_task(runnable_queue, address_book, contended_count, runnable_exclusive)
            {
                trace!("pop loop iteration");
                let from_runnable = task_source == TaskSource::Runnable;
                if from_runnable {
                    next_task.record_queue_time(*sequence_clock, *queue_clock);
                    *queue_clock = queue_clock.checked_add(1).unwrap();
                }
                let unique_weight = next_task.unique_weight;
                let message_hash = next_task.tx.0.message_hash();

                // plumb message_hash into StatusCache or implmenent our own for duplicate tx
                // detection?

                let (unlockable_count, provisional_count, busiest_page_cu) =
                    attempt_lock_for_execution(
                        ast,
                        from_runnable,
                        prefer_immediate,
                        address_book,
                        &unique_weight,
                        &message_hash,
                        &mut next_task.lock_attempts_mut(ast),
                    );

                if unlockable_count > 0 {
                    //trace!("reset_lock_for_failed_execution(): {:?} {}", (&unique_weight, from_runnable), next_task.tx.0.signature());
                    Self::reset_lock_for_failed_execution(
                        ast,
                        address_book,
                        &unique_weight,
                        &mut next_task.lock_attempts_mut(ast),
                    );
                    let lock_count = next_task.lock_attempts_mut(ast).len();
                    next_task
                        .contention_count
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                    if from_runnable {
                        trace!(
                            "move to contended due to lock failure [{}/{}/{}]",
                            unlockable_count,
                            provisional_count,
                            lock_count
                        );
                        next_task.mark_as_contended();
                        *contended_count = contended_count.checked_add(1).unwrap();

                        Task::index_with_address_book(ast, &next_task, task_sender);

                        // maybe run lightweight prune logic on contended_queue here.
                    } else {
                        trace!(
                            "relock failed [{}/{}/{}]; remains in contended: {:?} contention: {}",
                            unlockable_count,
                            provisional_count,
                            lock_count,
                            &unique_weight,
                            next_task
                                .contention_count
                                .load(std::sync::atomic::Ordering::SeqCst)
                        );
                        //address_book.uncontended_task_ids.clear();
                    }

                    if from_runnable || task_source == TaskSource::Stuck {
                        // for the case of being struck, we have already removed it from
                        // stuck_tasks, so pretend to add anew one.
                        // todo: optimize this needless operation
                        next_task.update_busiest_page_cu(busiest_page_cu);
                        /*
                        let a = address_book
                            .stuck_tasks
                            .insert(next_task.stuck_task_id(), Task::clone_in_queue(&next_task));
                        assert!(a.is_none());
                        */

                        if from_runnable {
                            // continue; // continue to prefer depleting the possibly-non-empty runnable queue
                            break;
                        } else if task_source == TaskSource::Stuck {
                            // need to bail out immediately to avoid going to infinite loop of re-processing
                            // the struck task again.
                            // todo?: buffer restuck tasks until readd to the stuck tasks until
                            // some scheduling state tick happens and try 2nd idling stuck task in
                            // the collection?
                            break;
                        } else {
                            unreachable!();
                        }
                    } else if task_source == TaskSource::Contended {
                        // todo: remove this task from stuck_tasks before update_busiest_page_cu
                        /*
                        let removed = address_book
                            .stuck_tasks
                            .remove(&next_task.stuck_task_id())
                            .unwrap();
                        next_task.update_busiest_page_cu(busiest_page_cu);
                        let a = address_book
                            .stuck_tasks
                            .insert(next_task.stuck_task_id(), removed);
                        assert!(a.is_none());
                        */
                        address_book.uncontended_task_ids.insert(next_task.unique_weight, next_task);
                        
                        break;
                    } else {
                        unreachable!();
                    }
                } else if provisional_count > 0 {
                    assert!(!from_runnable);
                    assert_eq!(unlockable_count, 0);
                    let lock_count = next_task.lock_attempts_mut(ast).len();
                    trace!("provisional exec: [{}/{}]", provisional_count, lock_count);
                    *contended_count = contended_count.checked_sub(1).unwrap();
                    next_task.mark_as_uncontended();
                    //address_book.stuck_tasks.remove(&next_task.stuck_task_id());
                    next_task.update_busiest_page_cu(busiest_page_cu);

                    let tracker = triomphe::Arc::new(ProvisioningTracker::new(
                        provisional_count,
                        Task::clone_in_queue(&next_task),
                    ));
                    *provisioning_tracker_count =
                        provisioning_tracker_count.checked_add(1).unwrap();
                    Self::finalize_lock_for_provisional_execution(
                        ast,
                        address_book,
                        &next_task,
                        tracker,
                    );

                    break;
                    //continue;
                }

                trace!(
                    "successful lock: (from_runnable: {}) after {} contentions",
                    from_runnable,
                    next_task
                        .contention_count
                        .load(std::sync::atomic::Ordering::SeqCst)
                );

                assert!(!next_task.already_finished());
                if !from_runnable {
                    *contended_count = contended_count.checked_sub(1).unwrap();
                    next_task.mark_as_uncontended();
                } else {
                    next_task.update_busiest_page_cu(busiest_page_cu);
                }
                let lock_attempts = std::mem::take(&mut *next_task.lock_attempts_mut(ast));

                return Some((unique_weight, next_task, lock_attempts));
            } else {
                break;
            }
        }

        None
    }

    #[inline(never)]
    fn finalize_lock_for_provisional_execution<AST: AtScheduleThread>(
        ast: AST,
        address_book: &mut AddressBook,
        next_task: &Task,
        tracker: triomphe::Arc<ProvisioningTracker>,
    ) {
        for l in next_task.lock_attempts_mut(ast).iter_mut() {
            match l.status {
                LockStatus::Provisional => {
                    l.target
                        .page_mut(ast)
                        .provisional_task_ids
                        .push(triomphe::Arc::clone(&tracker));
                }
                LockStatus::Succeded => {
                    // do nothing
                }
                LockStatus::Failed => {
                    unreachable!();
                }
            }
        }
        //trace!("provisioning_trackers: {}", address_book.provisioning_trackers.len());
    }

    #[inline(never)]
    fn reset_lock_for_failed_execution<AST: AtScheduleThread>(
        ast: AST,
        address_book: &mut AddressBook,
        unique_weight: &UniqueWeight,
        lock_attempts: &mut [LockAttempt],
    ) {
        for l in lock_attempts {
            address_book.reset_lock(ast, l, false);
        }
    }

    #[inline(never)]
    fn unlock_after_execution<AST: AtScheduleThread>(
        ast: AST,
        address_book: &mut AddressBook,
        lock_attempts: &mut [LockAttempt],
        provisioning_tracker_count: &mut usize,
        cu: CU,
    ) {
        for mut l in lock_attempts {
            let newly_uncontended = address_book.reset_lock(ast, &mut l, true);

            let mut page = l.target.page_mut(ast);
            page.cu += cu;
            if newly_uncontended && page.next_usage == Usage::Unused {
                //let mut inserted = false;

                if let Some(task) = l.heaviest_uncontended.take() {
                    //assert!(!task.already_finished());
                    if
                    /*true ||*/
                    task.currently_contended() {
                        //assert!(task.currently_contended());
                        //inserted = true;
                        address_book
                            .uncontended_task_ids
                            .insert(task.unique_weight, task);
                    } /*else {
                          let contended_unique_weights = &page.contended_unique_weights;
                          contended_unique_weights.heaviest_task_cursor().map(|mut task_cursor| {
                              let mut found = true;
                              //assert_ne!(task_cursor.key(), &task.uq);
                              let mut task = task_cursor.value();
                              while !task.currently_contended() {
                                  if let Some(new_cursor) = task_cursor.prev() {
                                      assert!(new_cursor.key() < task_cursor.key());
                                      //assert_ne!(new_cursor.key(), &uq);
                                      task_cursor = new_cursor;
                                      task = task_cursor.value();
                                  } else {
                                      found = false;
                                      break;
                                  }
                              }
                              found.then(|| Task::clone_in_queue(task))
                          }).flatten().map(|task| {
                              address_book.uncontended_task_ids.insert(task.unique_weight, task);
                              ()
                          });
                      }*/
                }
            }
            if page.current_usage == Usage::Unused && page.next_usage != Usage::Unused {
                page.switch_to_next_usage();
                for tracker in std::mem::take(&mut page.provisional_task_ids).into_iter() {
                    tracker.progress();
                    if tracker.is_fulfilled() {
                        trace!(
                            "provisioning tracker progress: {} => {} (!)",
                            tracker.prev_count(),
                            tracker.count()
                        );
                        address_book.fulfilled_provisional_task_ids.insert(
                            tracker.task.unique_weight,
                            Task::clone_in_queue(&tracker.task),
                        );
                        *provisioning_tracker_count =
                            provisioning_tracker_count.checked_sub(1).unwrap();
                    } else {
                        trace!(
                            "provisioning tracker progress: {} => {}",
                            tracker.prev_count(),
                            tracker.count()
                        );
                    }
                }
            }

            // todo: mem::forget and panic in LockAttempt::drop()
        }
    }

    #[inline(never)]
    fn prepare_scheduled_execution(
        address_book: &mut AddressBook,
        unique_weight: UniqueWeight,
        task: TaskInQueue,
        finalized_lock_attempts: Vec<LockAttempt>,
        queue_clock: &usize,
        execute_clock: &mut usize,
    ) -> Box<ExecutionEnvironment> {
        let mut rng = rand::thread_rng();
        // load account now from AccountsDb
        task.record_execute_time(*queue_clock, *execute_clock);
        *execute_clock = execute_clock.checked_add(1).unwrap();

        Box::new(ExecutionEnvironment {
            task,
            unique_weight,
            cu: rng.gen_range(3, 1000),
            finalized_lock_attempts,
            is_reindexed: Default::default(),
            execution_result: Default::default(),
        })
    }

    #[inline(never)]
    fn commit_processed_execution<AST: AtScheduleThread>(
        ast: AST,
        ee: &mut ExecutionEnvironment,
        address_book: &mut AddressBook,
        commit_clock: &mut usize,
        provisioning_tracker_count: &mut usize,
    ) {
        // do par()-ly?

        ee.reindex_with_address_book(ast);
        assert!(ee.is_reindexed());

        ee.task.record_commit_time(*commit_clock);
        //ee.task.trace_timestamps("commit");
        //assert_eq!(ee.task.execute_time(), *commit_clock);
        *commit_clock = commit_clock.checked_add(1).unwrap();

        // which order for data race free?: unlocking / marking
        Self::unlock_after_execution(
            ast,
            address_book,
            &mut ee.finalized_lock_attempts,
            provisioning_tracker_count,
            ee.cu,
        );
        ee.task.mark_as_finished();

        //address_book.stuck_tasks.remove(&ee.task.stuck_task_id());

        // block-wide qos validation will be done here
        // if error risen..:
        //   don't commit the tx for banking and potentially finish scheduling at block max cu
        //   limit
        //   mark the block as dead for replaying

        // par()-ly clone updated Accounts into address book
    }

    #[inline(never)]
    fn schedule_next_execution<AST: AtScheduleThread>(
        ast: AST,
        task_sender: &crossbeam_channel::Sender<(TaskInQueue, Vec<LockAttempt>)>,
        runnable_queue: &mut TaskQueue,
        address_book: &mut AddressBook,
        contended_count: &mut usize,
        prefer_immediate: bool,
        sequence_time: &usize,
        queue_clock: &mut usize,
        execute_clock: &mut usize,
        provisioning_tracker_count: &mut usize,
        runnable_exclusive: bool,
    ) -> Option<Box<ExecutionEnvironment>> {
        let maybe_ee = Self::pop_from_queue_then_lock(
            ast,
            task_sender,
            runnable_queue,
            address_book,
            contended_count,
            prefer_immediate,
            sequence_time,
            queue_clock,
            provisioning_tracker_count,
            runnable_exclusive,
        )
        .map(|(uw, t, ll)| {
            Self::prepare_scheduled_execution(address_book, uw, t, ll, queue_clock, execute_clock)
        });
        maybe_ee
    }

    #[inline(never)]
    fn register_runnable_task(
        weighted_tx: TaskInQueue,
        runnable_queue: &mut TaskQueue,
        sequence_time: &mut usize,
    ) {
        weighted_tx.record_sequence_time(*sequence_time);
        assert_eq!(*sequence_time, weighted_tx.transaction_index_in_entries_for_replay() as usize);
        *sequence_time = sequence_time.checked_add(1).unwrap();
        Self::push_to_runnable_queue(weighted_tx, runnable_queue)
    }

    fn _run<'a, AST: AtScheduleThread>(
        ast: AST,
        max_executing_queue_count: usize,
        runnable_queue: &mut TaskQueue,
        address_book: &mut AddressBook,
        mut from_prev: &'a crossbeam_channel::Receiver<SchedulablePayload>,
        to_execute_substage: &crossbeam_channel::Sender<ExecutablePayload>,
        from_exec: &crossbeam_channel::Receiver<UnlockablePayload>,
        maybe_to_next_stage: Option<&crossbeam_channel::Sender<ExaminablePayload>>, // assume nonblocking
        never: &'a crossbeam_channel::Receiver<SchedulablePayload>,
    ) {
        let random_id = rand::thread_rng().gen::<u64>();
        info!("schedule_once:initial id_{:016x}", random_id);

        let mut executing_queue_count = 0_usize;
        let mut contended_count = 0;
        let mut provisioning_tracker_count = 0;
        let mut sequence_time = 0;
        let mut queue_clock = 0;
        let mut execute_clock = 0;
        let mut commit_clock = 0;
        let mut processed_count = 0_usize;
        let mut interval_count = 0;

        assert!(max_executing_queue_count > 0);

        let (ee_sender, ee_receiver) = crossbeam_channel::unbounded::<ExaminablePayload>();

        let (to_next_stage, maybe_reaper_thread_handle) = if let Some(to_next_stage) = maybe_to_next_stage {
            (to_next_stage, None)
        } else {
            let h = std::thread::Builder::new()
                .name("solScReaper".to_string())
                .spawn(move || {
                    #[derive(Clone, Copy, Debug)]
                    struct NotAtTopOfScheduleThread;
                    unsafe impl NotAtScheduleThread for NotAtTopOfScheduleThread {}
                    let nast = NotAtTopOfScheduleThread;

                    while let Ok(ExaminablePayload(mut a)) = ee_receiver.recv() {
                        assert!(a.task.lock_attempts_not_mut(nast).is_empty());
                        //assert!(a.task.sequence_time() != usize::max_value());
                        //let lock_attempts = std::mem::take(&mut a.lock_attempts);
                        //drop(lock_attempts);
                        //TaskInQueue::get_mut(&mut a.task).unwrap();
                    }
                    assert_eq!(ee_receiver.len(), 0);
                    Ok::<(), ()>(())
                })
                .unwrap();

            (&ee_sender, Some(h))
        };
        let (task_sender, task_receiver) =
            crossbeam_channel::unbounded::<(TaskInQueue, Vec<LockAttempt>)>();
        let indexer_count = std::env::var("INDEXER_COUNT")
            .unwrap_or(format!("{}", 4))
            .parse::<usize>()
            .unwrap();
        let indexer_handles = (0..indexer_count).map(|thx| {
            let task_receiver = task_receiver.clone();
            std::thread::Builder::new()
                .name(format!("solScIdxer{:02}", thx))
                .spawn(move || {
                    while let Ok((task, ll)) = task_receiver.recv() {
                        for lock_attempt in ll {
                            if task.already_finished() {
                                break;
                            }
                            lock_attempt
                                .target_contended_unique_weights()
                                .insert_task(task.unique_weight, Task::clone_in_queue(&task));
                            todo!("contended_write_task_count!");
                        }
                    }
                    assert_eq!(task_receiver.len(), 0);
                    Ok::<(), ()>(())
                })
                .unwrap()
        }).collect::<Vec<_>>();
        let (mut last_time, mut last_processed_count) = (std::time::Instant::now(), 0_usize);
        let start_time = last_time.clone();

        let (mut from_disconnected, mut from_exec_disconnected, mut no_more_work): (bool, bool, bool) = Default::default();
        loop {
            if !from_disconnected || executing_queue_count >= 1 {
            crossbeam_channel::select! {
               recv(from_exec) -> maybe_from_exec => {
                   if let Ok(UnlockablePayload(mut processed_execution_environment)) = maybe_from_exec {
                       executing_queue_count = executing_queue_count.checked_sub(1).unwrap();
                       processed_count = processed_count.checked_add(1).unwrap();
                       Self::commit_processed_execution(ast, &mut processed_execution_environment, address_book, &mut commit_clock, &mut provisioning_tracker_count);
                       to_next_stage.send(ExaminablePayload(processed_execution_environment)).unwrap();
                   } else {
                       assert_eq!(from_exec.len(), 0);
                       from_exec_disconnected |= true;
                       info!("flushing1..: {:?} {} {} {} {}", (from_disconnected, from_exec_disconnected), runnable_queue.task_count(), contended_count, executing_queue_count, provisioning_tracker_count);
                       if from_disconnected {
                           break;
                       }
                   }
               }
               recv(from_prev) -> maybe_from => {
                   if let Ok(SchedulablePayload(task)) = maybe_from {
                       Self::register_runnable_task(task, runnable_queue, &mut sequence_time);
                   } else {
                       assert_eq!(from_prev.len(), 0);
                       from_disconnected |= true;
                       from_prev = never;
                       info!("flushing2..: {:?} {} {} {} {}", (from_disconnected, from_exec_disconnected), runnable_queue.task_count(), contended_count, executing_queue_count, provisioning_tracker_count);
                   }
               }
            }
            }

           no_more_work = from_disconnected && runnable_queue.task_count() + contended_count + executing_queue_count + provisioning_tracker_count == 0;
           if from_disconnected && (from_exec_disconnected || no_more_work) {
               break;
           }

            let mut first_iteration = true;
            let (mut empty_from, mut empty_from_exec) = (false, false);
            let (mut from_len, mut from_exec_len) = (0, 0);

            loop {
                let executing_like_count = executing_queue_count + provisioning_tracker_count;
                if executing_like_count < max_executing_queue_count {
                    let prefer_immediate = true; //provisioning_tracker_count / 4 > executing_queue_count;

                    if let Some(ee) = Self::schedule_next_execution(
                        ast,
                        &task_sender,
                        runnable_queue,
                        address_book,
                        &mut contended_count,
                        prefer_immediate,
                        &sequence_time,
                        &mut queue_clock,
                        &mut execute_clock,
                        &mut provisioning_tracker_count,
                        false,
                    ) {
                        executing_queue_count = executing_queue_count.checked_add(1).unwrap();
                        to_execute_substage.send(ExecutablePayload(ee)).unwrap();
                    }
                    debug!("schedule_once id_{:016x} [C] ch(prev: {}, exec: {}|{}), r: {}, u/c: {}/{}, (imm+provi)/max: ({}+{})/{} s: {} done: {}", random_id, from_prev.len(), to_execute_substage.len(), from_exec.len(), runnable_queue.task_count(), address_book.uncontended_task_ids.len(), contended_count, executing_queue_count, provisioning_tracker_count, max_executing_queue_count, address_book.stuck_tasks.len(), processed_count);
                    interval_count += 1;
                    if interval_count % 100 == 0 {
                        let elapsed = last_time.elapsed();
                        if elapsed > std::time::Duration::from_millis(150) {
                            let delta = (processed_count - last_processed_count) as u128;
                            let elapsed2 = elapsed.as_micros();
                            info!("schedule_once:interval id_{:016x} ch(prev: {}, exec: {}|{}), r: {}, u/c: {}/{}, (imm+provi)/max: ({}+{})/{} s: {} done: {} ({}txs/{}us={}tps)", random_id, from_prev.len(), to_execute_substage.len(), from_exec.len(), runnable_queue.task_count(), address_book.uncontended_task_ids.len(), contended_count, executing_queue_count, provisioning_tracker_count, max_executing_queue_count, address_book.stuck_tasks.len(), processed_count, delta, elapsed.as_micros(), 1_000_000_u128*delta/elapsed2);
                            (last_time, last_processed_count) = (std::time::Instant::now(), processed_count);
                        }
                    }
                }
                while executing_queue_count + provisioning_tracker_count < max_executing_queue_count {
                    let prefer_immediate = true; //provisioning_tracker_count / 4 > executing_queue_count;

                    let maybe_ee = Self::schedule_next_execution(
                        ast,
                        &task_sender,
                        runnable_queue,
                        address_book,
                        &mut contended_count,
                        prefer_immediate,
                        &sequence_time,
                        &mut queue_clock,
                        &mut execute_clock,
                        &mut provisioning_tracker_count,
                        true
                    );
                    if let Some(ee) = maybe_ee {
                        executing_queue_count = executing_queue_count.checked_add(1).unwrap();
                        to_execute_substage.send(ExecutablePayload(ee)).unwrap();
                        debug!("schedule_once id_{:016x} [R] ch(prev: {}, exec: {}|{}), r: {}, u/c: {}/{}, (imm+provi)/max: ({}+{})/{} s: {} done: {}", random_id, from_prev.len(), to_execute_substage.len(), from_exec.len(), runnable_queue.task_count(), address_book.uncontended_task_ids.len(), contended_count, executing_queue_count, provisioning_tracker_count, max_executing_queue_count, address_book.stuck_tasks.len(), processed_count);
                    } else {
                        debug!("schedule_once id_{:016x} [R] ch(prev: {}, exec: {}|{}), r: {}, u/c: {}/{}, (imm+provi)/max: ({}+{})/{} s: {} done: {}", random_id, from_prev.len(), to_execute_substage.len(), from_exec.len(), runnable_queue.task_count(), address_book.uncontended_task_ids.len(), contended_count, executing_queue_count, provisioning_tracker_count, max_executing_queue_count, address_book.stuck_tasks.len(), processed_count);
                        break;
                    }
                    if !from_exec.is_empty() {
                        trace!("abort aggressive readable queue processing due to non-empty from_exec");
                        break;
                    }

                    interval_count += 1;
                    if interval_count % 100 == 0 {
                        let elapsed = last_time.elapsed();
                        if elapsed > std::time::Duration::from_millis(150) {
                            let delta = (processed_count - last_processed_count) as u128;
                            let elapsed2 = elapsed.as_micros();
                            info!("schedule_once:interval id_{:016x} ch(prev: {}, exec: {}|{}), r: {}, u/c: {}/{}, (imm+provi)/max: ({}+{})/{} s: {} done: {} ({}txs/{}us={}tps)", random_id, from_prev.len(), to_execute_substage.len(), from_exec.len(), runnable_queue.task_count(), address_book.uncontended_task_ids.len(), contended_count, executing_queue_count, provisioning_tracker_count, max_executing_queue_count, address_book.stuck_tasks.len(), processed_count, delta, elapsed.as_micros(), 1_000_000_u128*delta/elapsed2);
                            (last_time, last_processed_count) = (std::time::Instant::now(), processed_count);
                        }
                    }
                }

                if first_iteration {
                    first_iteration = false;
                    (from_len, from_exec_len) = (from_prev.len(), from_exec.len());
                } else {
                    if empty_from {
                        from_len = from_prev.len();
                    }
                    if empty_from_exec {
                        from_exec_len = from_exec.len();
                    }
                }
                (empty_from, empty_from_exec) = (from_len == 0, from_exec_len == 0);

                if empty_from && empty_from_exec {
                    break;
                } else {
                    if !empty_from_exec {
                        let mut processed_execution_environment = from_exec.recv().unwrap().0;
                        from_exec_len = from_exec_len.checked_sub(1).unwrap();
                        empty_from_exec = from_exec_len == 0;
                        executing_queue_count = executing_queue_count.checked_sub(1).unwrap();
                        processed_count = processed_count.checked_add(1).unwrap();
                        Self::commit_processed_execution(
                            ast,
                            &mut processed_execution_environment,
                            address_book,
                            &mut commit_clock,
                            &mut provisioning_tracker_count,
                        );
                        to_next_stage.send(ExaminablePayload(processed_execution_environment)).unwrap();
                    }
                    if !empty_from {
                        let task = from_prev.recv().unwrap().0;
                        from_len = from_len.checked_sub(1).unwrap();
                        empty_from = from_len == 0;
                        Self::register_runnable_task(task, runnable_queue, &mut sequence_time);
                    }
                }
            }
        }
        drop(to_next_stage);
        drop(ee_sender);
        drop(task_sender);
        drop(task_receiver);
        info!("run finished...");
        if let Some(h) = maybe_reaper_thread_handle {
            h.join().unwrap().unwrap();
        }
        for indexer_handle in indexer_handles {
            indexer_handle.join().unwrap().unwrap();
        }


        let elapsed2 = start_time.elapsed().as_micros();
        info!("schedule_once:final id_{:016x} (from_disconnected: {}, from_exec_disconnected: {}, no_more_work: {}) ch(prev: {}, exec: {}|{}), runnnable: {}, contended: {}, (immediate+provisional)/max: ({}+{})/{} uncontended: {} stuck: {} overall: {}txs/{}us={}tps!", random_id, from_disconnected, from_exec_disconnected, no_more_work, from_prev.len(), to_execute_substage.len(), from_exec.len(), runnable_queue.task_count(), contended_count, executing_queue_count, provisioning_tracker_count, max_executing_queue_count, address_book.uncontended_task_ids.len(), address_book.stuck_tasks.len(), processed_count, elapsed.as_micros(), 1_000_000_u128*processed_count/elapsed2);
    }

    pub fn run(
        max_executing_queue_count: usize,
        runnable_queue: &mut TaskQueue,
        address_book: &mut AddressBook,
        from: &crossbeam_channel::Receiver<SchedulablePayload>,
        to_execute_substage: &crossbeam_channel::Sender<ExecutablePayload>,
        from_execute_substage: &crossbeam_channel::Receiver<UnlockablePayload>,
        maybe_to_next_stage: Option<&crossbeam_channel::Sender<ExaminablePayload>>, // assume nonblocking
    ) {
        #[derive(Clone, Copy, Debug)]
        struct AtTopOfScheduleThread;
        unsafe impl AtScheduleThread for AtTopOfScheduleThread {}

        Self::_run::<AtTopOfScheduleThread>(
            AtTopOfScheduleThread,
            max_executing_queue_count,
            runnable_queue,
            address_book,
            from,
            to_execute_substage,
            from_execute_substage,
            maybe_to_next_stage,
            &crossbeam_channel::never(),
        )
    }
}

pub struct SchedulablePayload(pub TaskInQueue);
pub struct ExecutablePayload(pub Box<ExecutionEnvironment>);
pub struct UnlockablePayload(pub Box<ExecutionEnvironment>);
pub struct ExaminablePayload(pub Box<ExecutionEnvironment>);

struct ExecuteStage {
    //bank: Bank,
}

impl ExecuteStage {}
