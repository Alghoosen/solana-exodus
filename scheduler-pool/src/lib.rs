use {
    log::*,
};
use std::time::Duration;
use solana_runtime::bank::TransactionResults;
use std::sync::Arc;
use solana_runtime::transaction_batch::TransactionBatch;
use solana_runtime::bank::Bank;
use solana_runtime::bank::LoadAndExecuteTransactionsOutput;
use solana_runtime::bank::CommitTransactionCounts;
use std::borrow::Cow;
use solana_runtime::bank::LikeScheduler;
use solana_runtime::bank_forks::LikeSchedulerPool;
use solana_runtime::bank::SchedulerContext;
use std::sync::atomic::AtomicBool;
use solana_sdk::transaction::SanitizedTransaction;
use solana_sdk::transaction::Result;
use solana_program_runtime::timings::ExecuteTimings;
use solana_runtime::transaction_priority_details::GetTransactionPriorityDetails;
use std::collections::HashMap;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::clock::MAX_PROCESSING_AGE;
use solana_sdk::transaction::TransactionError;
use std::time::Instant;
use solana_sdk::transaction::VersionedTransaction;
use solana_metrics::inc_new_counter_info;
use solana_metrics::inc_new_counter;
use solana_metrics::inc_counter;
use solana_metrics::create_counter;
use solana_measure::measure::Measure;
use std::sync::RwLock;
use solana_poh::poh_recorder::PohRecorder;
use solana_poh::poh_recorder::TransactionRecorder;
use assert_matches::assert_matches;
use solana_transaction_status::token_balances::TransactionTokenBalancesSet;
use solana_runtime::bank::TransactionBalancesSet;
use solana_runtime::vote_sender_types::ReplayVoteSender;
use solana_ledger::blockstore_processor::TransactionStatusSender;
use solana_ledger::token_balances::collect_token_balances;
use solana_scheduler::WithMode;
use solana_scheduler::WithContext;

pub use solana_scheduler::Mode;

#[derive(Debug)]
pub struct SchedulerPool {
    schedulers: std::sync::Mutex<Vec<Box<dyn LikeScheduler>>>,
    log_messages_bytes_limit: Option<usize>,
    transaction_status_sender: Option<TransactionStatusSender>,
    replay_vote_sender: Option<ReplayVoteSender>,
}

impl SchedulerPool {
    fn new(poh_recorder: Option<&Arc<RwLock<PohRecorder>>>, log_messages_bytes_limit: Option<usize>, transaction_status_sender: Option<TransactionStatusSender>, replay_vote_sender: Option<ReplayVoteSender>) -> Self {
        Self {
            schedulers: std::sync::Mutex::new(Vec::new()),
            log_messages_bytes_limit,
            transaction_status_sender,
            replay_vote_sender,
        }
    }

    fn prepare_new_scheduler(self: &Arc<Self>, context: SchedulerContext) {
        // block on some max count of borrowed schdulers!
        self.schedulers.lock().unwrap().push(Box::new(Scheduler::spawn(self.clone(), context)));
    }

    pub fn new_boxed(poh_recorder: Option<&Arc<RwLock<PohRecorder>>>, log_messages_bytes_limit: Option<usize>, transaction_status_sender: Option<TransactionStatusSender>, replay_vote_sender: Option<ReplayVoteSender>) -> Box<dyn LikeSchedulerPool> {
        Box::new(SchedulerPoolWrapper::new(poh_recorder, log_messages_bytes_limit, transaction_status_sender, replay_vote_sender))
    }
}

impl Drop for SchedulerPool {
    fn drop(&mut self) {
        let current_thread_name = std::thread::current().name().unwrap().to_string();
        warn!("SchedulerPool::drop() by {}...", current_thread_name);
        todo!();
        //info!("Scheduler::drop(): id_{:016x} begin..", self.random_id);
        //self.gracefully_stop().unwrap();
        //info!("Scheduler::drop(): id_{:016x} end...", self.random_id);
    }
}

#[derive(Debug)]
struct SchedulerPoolWrapper(Arc<SchedulerPool>);

impl SchedulerPoolWrapper {
    fn new(poh_recorder: Option<&Arc<RwLock<PohRecorder>>>, log_messages_bytes_limit: Option<usize>, transaction_status_sender: Option<TransactionStatusSender>, replay_vote_sender: Option<ReplayVoteSender>) -> Self {
        Self(Arc::new(SchedulerPool::new(poh_recorder, log_messages_bytes_limit, transaction_status_sender, replay_vote_sender)))
    }
}

impl SchedulerPool {
    fn take_from_pool(self: &Arc<Self>, context: Option<SchedulerContext>) -> Box<dyn LikeScheduler> {
        let mut schedulers = self.schedulers.lock().unwrap();
        let maybe_scheduler = schedulers.pop();
        if let Some(scheduler) = maybe_scheduler {
            trace!(
                "SchedulerPool: id_{:016x} is taken... len: {} => {}",
                scheduler.random_id(),
                schedulers.len() + 1,
                schedulers.len()
            );
            drop(schedulers);

            if let Some(context) = context {
                scheduler.replace_scheduler_context(context);
            }
            scheduler
        } else {
            drop(schedulers);

            self.prepare_new_scheduler(context.unwrap());
            self.take_from_pool(None)
        }
    }

    fn return_to_pool(self: &Arc<Self>, mut scheduler: Box<dyn LikeScheduler>) {
        let mut schedulers = self.schedulers.lock().unwrap();

        trace!(
            "SchedulerPool: id_{:016x} is returned... len: {} => {}",
            scheduler.random_id(),
            schedulers.len(),
            schedulers.len() + 1
        );
        scheduler.clear_stop();

        schedulers.push(scheduler);
    }
}

impl LikeSchedulerPool for SchedulerPoolWrapper {
    fn take_from_pool(&self, context: SchedulerContext) -> Box<dyn LikeScheduler> {
        self.0.take_from_pool(Some(context))
    }

    fn return_to_pool(&self, scheduler: Box<dyn LikeScheduler>) {
        self.0.return_to_pool(scheduler);
    }
}

use solana_transaction_status::TransactionTokenBalance;

#[derive(Debug)]
pub(crate) struct Scheduler {
    random_id: u64,
    scheduler_thread_handle: Option<std::thread::JoinHandle<Result<(Duration, Duration)>>>,
    executing_thread_handles: Option<Vec<std::thread::JoinHandle<Result<(Duration, Duration)>>>>,
    error_collector_thread_handle: Option<std::thread::JoinHandle<Result<(Duration, Duration)>>>,
    transaction_sender: Option<crossbeam_channel::Sender<solana_scheduler::SchedulablePayload>>,
    preloader: Arc<solana_scheduler::Preloader>,
    graceful_stop_initiated: bool,
    collected_results: Arc<std::sync::Mutex<Option<(ExecuteTimings, <Result<()>>)>>>,
    commit_status: Arc<CommitStatus>,
    checkpoint: Arc<Checkpoint>,
    stopped_mode: Option<solana_scheduler::Mode>,
    current_scheduler_context: RwLock<Option<SchedulerContext>>,
    thread_count: usize,
    scheduler_pool: Arc<SchedulerPool>, // use Weak to cut circuric dep.
}

#[derive(Debug)]
struct CommitStatus {
    is_paused: std::sync::Mutex<(bool, usize)>, // maybe should use blockheight: u64 to avoid race for races between replay and executor's poh error?
    condvar: std::sync::Condvar,
}

impl CommitStatus {
    fn new() -> Self {
        Self {
            is_paused: Default::default(),
            condvar: Default::default(),
        }
    }

    fn check_and_wait(&self, random_id: u64, current_thread_name: &str, last_seq: &mut usize, context: &mut Option<SchedulerContext>) {
        let mut is_paused = self.is_paused.lock().unwrap();
        if *last_seq != is_paused.1 {
            *last_seq = is_paused.1;
            if let Some(sc) = context.take() {
                info!("CommitStatus: {current_thread_name} {} detected stale scheduler_context...", SchedulerContext::log_prefix(random_id, Some(&sc)));
                // drop arc in scheduler_context as soon as possible
                drop(sc);
            }
        }

        if !is_paused.0 {
            return
        }

        info!("CommitStatus: {current_thread_name} is paused...");
        self.condvar.wait_while(is_paused, |now_is_paused| now_is_paused.0).unwrap();
        info!("CommitStatus: {current_thread_name} is resumed...");
    }

    fn notify_as_paused(&self) {
        let current_thread_name = std::thread::current().name().unwrap().to_string();
        let mut is_paused = self.is_paused.lock().unwrap();
        if is_paused.0 {
            info!("CommitStatus: {current_thread_name} is skipped to notify as paused...");
        } else {
            info!("CommitStatus: {current_thread_name} is notifying as paused...");
            is_paused.0 = true;
            is_paused.1 += 1;
        }
    }

    fn notify_as_resumed(&self) {
        let current_thread_name = std::thread::current().name().unwrap().to_string();
        let mut is_paused = self.is_paused.lock().unwrap();
        if is_paused.0 {
            info!("CommitStatus: {current_thread_name} is notifying as resumed...");
            is_paused.0 = false;
            self.condvar.notify_all();
        }
    }
}

#[derive(Debug)]
pub struct Checkpoint(std::sync::Mutex<((usize, usize), Option<(ExecuteTimings, Result<()>>, Option<SchedulerContext>, usize)>, std::sync::Condvar, std::sync::Condvar, usize);

impl Checkpoint {
    pub fn wait_for_restart(&self) {
        let mut a = &mut None;
        let mut current_thread_name = || a.get_or_insert_with(|| std::thread::current().name().unwrap().to_string()).clone() ;

        let mut g = self.0.lock().unwrap();
        let ((threads_before_checkpoint, threads_after_checkpoint), self_return_value, _, context_count) = &mut *g;
        info!(
            "Checkpoint::wait_for_restart: {} is entering at {} -> {}",
            current_thread_name(),
            *threads_before_checkpoint,
            *threads_before_checkpoint - 1
        );

        *threads_before_checkpoint = threads_before_checkpoint.checked_sub(1).unwrap();

        if *threads_before_checkpoint == 0 {
            assert!(self_return_value.is_some());
            assert!(*threads_after_checkpoint <= 1);
            *threads_after_checkpoint = threads_after_checkpoint.checked_add(1).unwrap();
            drop((threads_before_checkpoint, threads_after_checkpoint));
            assert_eq!(*context_count, 0);
            self.1.notify_all();
            info!(
                "Checkpoint::wait_for_restart: {} notified all others...",
                current_thread_name()
            );
        } else {
            info!(
                "Checkpoint::wait_for_restart: {} is paused...",
                current_thread_name()
            );
            let _ = *self
                .1
                .wait_while(g, |((threads_before_checkpoint, threads_after_checkpoint), ..)| {
                    if *threads_before_checkpoint > 0 {
                        true
                    } else {
                        *threads_after_checkpoint = threads_after_checkpoint.checked_add(1).unwrap();
                        if *threads_after_checkpoint == self.thread_count() {
                            self.2.notify_one();
                        }
                        info!(
                            "Checkpoint::wait_for_restart: {} is started... {threads_after_checkpoint}",
                            current_thread_name()
                        );
                        false
                    }
                })
                .unwrap();
        }
    }

    fn wait_for_completed_restart(&self) {
        let mut a = &mut None;
        let mut current_thread_name = || a.get_or_insert_with(|| std::thread::current().name().unwrap().to_string()).clone() ;

        let mut g = self.0.lock().unwrap();
        let ((threads_before_checkpoint, threads_after_checkpoint), self_return_value, _, context_count) = &mut *g;
        assert_eq!(*threads_before_checkpoint, 0);
        let mut is_waited = false;
        let _ = *self
            .2
            .wait_while(g, |(counter_values, ..)| {
                let (_, threads_after_checkpoint) = &counter_values;
                if *threads_after_checkpoint < self.thread_count() {
                    if !is_waited {
                        is_waited = true;
                        info!(
                            "Checkpoint::wait_for_completed_restart: {} is waited... {threads_after_checkpoint}",
                            current_thread_name()
                        );
                    }
                    true
                } else {
                    assert_eq!(*counter_values, Self::final_counter_values(self.thread_count()));
                    *counter_values = Self::initial_counter_values(self.thread_count());
                    if is_waited {
                        info!(
                            "Checkpoint::wait_for_completed_restart: {} is notified...",
                            current_thread_name()
                        );
                    } else {
                        info!(
                            "Checkpoint::wait_for_completed_restart: {} is reset",
                            current_thread_name()
                        );
                    }
                    false
                }
            })
            .unwrap();
    }

    fn wait_for_restart_from_internal_thread(&self, scheduler_context: Option<SchedulerContext>) {
        let did_drop = if let Some(scheduler_context) = scheduler_context {
            scheduler_context.drop_cyclically()
        } else {
            false
        };
        if !did_drop {
            self.wait_for_restart();
        }
    }

    fn thread_count(&self) -> usize {
        self.3
    }

    fn initial_counter_values(thread_count: usize) -> (usize, usize) {
        (thread_count, 0)
    }

    fn final_counter_values(thread_count: usize) -> (usize, usize) {
        (0, thread_count)
    }

    pub fn register_return_value(&self, restart_value: (ExecuteTimings, Result<()>)) {
        let mut g = self.0.lock().unwrap();
        let (_, self_return_value, ..) = &mut *g;
        assert!(self_return_value.is_none());
        *self_return_value = Some(restart_value);
    }

    pub fn ignore_external_thread(&self) {
        let current_thread_name = std::thread::current().name().unwrap().to_string();
        let mut g = self.0.lock().unwrap();
        let ((threads_before_checkpoint, threads_after_checkpoint), ..) = &mut *g;
        info!(
            "Checkpoint::ignore_external_thread: {} is entering at {} -> {}",
            current_thread_name,
            *threads_before_checkpoint,
            *threads_before_checkpoint - 1
        );

        assert_eq!(*threads_after_checkpoint, 0);
        *threads_before_checkpoint = threads_before_checkpoint.checked_sub(1).unwrap();
        *threads_after_checkpoint = threads_after_checkpoint.checked_add(1).unwrap();
        assert!(*threads_before_checkpoint >= 1);
    }

    pub fn take_restart_value(&self) -> (ExecuteTimings, Result<()>) {
        let mut g = self.0.lock().unwrap();
        let (_, self_return_value, ..) = &mut *g;
        self_return_value.take().unwrap()
    }

    pub fn new(thread_count: usize) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self(
            std::sync::Mutex::new((Self::initial_counter_values(thread_count), None, None, 0)),
            std::sync::Condvar::new(),
            std::sync::Condvar::new(),
            thread_count,
        ))
    }

    pub fn replace_context_value(&self, new: SchedulerContext) {
        let mut g = self.0.lock().unwrap();
        let (_, self_return_value, b, context_count) = &mut *g;
        assert_eq!(*context_count, 0);
        *context_count = self.thread_count();
        *b = Some(new);
    }
}

impl solana_scheduler::WithContext for Checkpoint {
    type Context = SchedulerContext;

    fn use_context_value(&self) -> Option<SchedulerContext> {
        let mut a = &mut None;
        let mut current_thread_name = || a.get_or_insert_with(|| std::thread::current().name().unwrap().to_string()).clone() ;

        let mut g = self.0.lock().unwrap();
        let (_, self_return_value, b, context_count) = &mut *g;
        *context_count = context_count.checked_sub(1).unwrap();
        let c = if *context_count > 0 {
            info!(
                "Checkpoint::use_context_value: {} used ({})",
                current_thread_name(),
                *context_count,
            );
            b.clone()
        } else {
            info!(
                "Checkpoint::use_context_value: {} took ({})",
                current_thread_name(),
                *context_count,
            );
            b.take()
        };
        assert!(c.is_some());
        c
    }
}


impl Scheduler {
    fn spawn(scheduler_pool: Arc<SchedulerPool>, initial_context: SchedulerContext) -> Self {
        let start = Instant::now();
        let mut address_book = solana_scheduler::AddressBook::default();
        let preloader = Arc::new(address_book.preloader());
        let (transaction_sender, transaction_receiver) = crossbeam_channel::unbounded();
        let (scheduled_ee_sender, scheduled_ee_receiver) = crossbeam_channel::unbounded();
        let (scheduled_high_ee_sender, scheduled_high_ee_receiver) = crossbeam_channel::unbounded();
        let (processed_ee_sender, processed_ee_receiver) = crossbeam_channel::unbounded();
        let (retired_ee_sender, retired_ee_receiver) = crossbeam_channel::unbounded();


        let executing_thread_count = std::env::var("EXECUTING_THREAD_COUNT")
            .unwrap_or(format!("{}", 8))
            .parse::<usize>()
            .unwrap();
        let base_thread_count = executing_thread_count / 2;
        let thread_count = 3 + executing_thread_count;
        let checkpoint = {
            let mut c = Self::new_checkpoint(thread_count);
            c.replace_context_value(initial_context);
            c
        };

        let max_thread_priority = std::env::var("MAX_THREAD_PRIORITY").is_ok();
        let commit_status = Arc::new(CommitStatus::new());

        use rand::Rng;
        let random_id = rand::thread_rng().gen::<u64>();

        let executing_thread_count = std::cmp::max(base_thread_count * 2, 1);
        let executing_thread_handles = (0..executing_thread_count).map(|thx| {
            let (scheduled_ee_receiver, scheduled_high_ee_receiver, processed_ee_sender) = (scheduled_ee_receiver.clone(), scheduled_high_ee_receiver.clone(), processed_ee_sender.clone());
            let checkpoint = checkpoint.clone();
            let commit_status = commit_status.clone();
            let scheduler_pool = scheduler_pool.clone();
            let thread_name = format!("solScExLane{:02}", thx);

            std::thread::Builder::new().name(thread_name.clone()).spawn(move || {
            let mut mint_decimals: HashMap<Pubkey, u8> = HashMap::new();

            let started = (cpu_time::ThreadTime::now(), std::time::Instant::now());
            if max_thread_priority {
                thread_priority::set_current_thread_priority(thread_priority::ThreadPriority::Max).unwrap();
            }
            let mut latest_seq = 0;
            let (mut latest_scheduler_context, mut mode) = (None, None);

            'recv: while let Ok(r) = (if thx >= base_thread_count { scheduled_high_ee_receiver.recv() } else { scheduled_ee_receiver.recv()}) {
                if latest_scheduler_context.is_none() {
                    latest_scheduler_context = checkpoint.use_context_value();
                    mode = latest_scheduler_context.as_ref().map(|sc| sc.mode);
                }
                match r {
                solana_scheduler::ExecutablePayload(solana_scheduler::Flushable::Payload(mut ee)) => {

                'retry: loop {
                let Some(bank) = latest_scheduler_context.as_ref().map(|sc| sc.bank()) else {
                    match mode {
                        Some(solana_scheduler::Mode::Replaying) => panic!(),
                        None => (),
                    };
                    processed_ee_sender.send(solana_scheduler::UnlockablePayload(ee, Default::default())).unwrap();
                    continue 'recv;
                };
                let mode = mode.unwrap();

                let (mut wall_time, cpu_time) = (Measure::start("process_message_time"), cpu_time::ThreadTime::now());

                let current_execute_clock = ee.task.execute_time();
                let transaction_index = ee.task.transaction_index(mode);
                trace!("execute_substage: transaction_index: {} execute_clock: {} at thread: {}", thx, transaction_index, current_execute_clock);

                let slot = bank.slot();

                let tx_account_lock_limit = bank.get_transaction_account_lock_limit();
                let lock_result = ee.task.tx.0
                    .get_account_locks(tx_account_lock_limit)
                    .map(|_| ());
                let mut batch =
                    TransactionBatch::new(vec![lock_result], &bank, Cow::Owned(vec![ee.task.tx.0.clone()]));
                batch.set_needs_unlock(false);
                let bb = scheduler_pool.transaction_status_sender.as_ref().map(|sender|
                    send_transaction_status(sender, None, &bank, &batch, &mut mint_decimals, None, None)
                );

                let mut timings = Default::default();
                let LoadAndExecuteTransactionsOutput {
                    mut loaded_transactions,
                    mut execution_results,
                    mut executed_transactions_count,
                    executed_non_vote_transactions_count,
                    executed_with_successful_result_count,
                    mut signature_count,
                    ..
                } = bank.load_and_execute_transactions(
                    &batch,
                    MAX_PROCESSING_AGE,
                    bb.is_some(),
                    bb.is_some(),
                    bb.is_some(),
                    &mut timings,
                    None,
                    scheduler_pool.log_messages_bytes_limit,
                );

                let (last_blockhash, lamports_per_signature) =
                    bank.last_blockhash_and_lamports_per_signature();

                let commited_first_transaction_index = match mode {
                    solana_scheduler::Mode::Replaying => {
                        //info!("replaying commit! {slot}");
                        Some(ee.task.transaction_index(mode) as usize)
                   },
                };

                let tx_results = bank.commit_transactions(
                    batch.sanitized_transactions(),
                    &mut loaded_transactions,
                    execution_results,
                    last_blockhash,
                    lamports_per_signature,
                    CommitTransactionCounts {
                        committed_transactions_count: executed_transactions_count as u64,
                        committed_with_failure_result_count: executed_transactions_count
                            .saturating_sub(executed_with_successful_result_count)
                            as u64,
                        committed_non_vote_transactions_count: executed_non_vote_transactions_count as u64,
                        signature_count,
                    },
                    &mut timings,
                );

                let TransactionResults {
                    fee_collection_results,
                    execution_results,
                    ..
                } = &tx_results;

                let tx_result = fee_collection_results.clone().into_iter().collect::<Result<_>>();
                if tx_result.is_ok() {
                    let details = execution_results[0].details().unwrap();
                    ee.cu = details.executed_units;
                } else {
                    let sig = || ee.task.tx.0.signature().to_string();
                    match mode {
                        solana_scheduler::Mode::Replaying => {
                            error!("found odd tx error: slot: {}, signature: {}, {:?}", slot, sig(), tx_result);
                        },
                    }
                };

                ee.execution_result = Some(tx_result);
                ee.finish_time = Some(std::time::SystemTime::now());
                ee.thx = thx;
                ee.execution_cpu_us = cpu_time.elapsed().as_micros();
                // make wall time is longer than cpu time, always
                wall_time.stop();
                ee.execution_us = wall_time.as_us();

                solana_runtime::bank_utils::find_and_send_votes(
                    batch.sanitized_transactions(),
                    &tx_results,
                    scheduler_pool.replay_vote_sender.as_ref(),
                );

                if let Some(commited_first_transaction_index) = commited_first_transaction_index {
                    if let Some(bb) = bb {
                        assert!(send_transaction_status(scheduler_pool.transaction_status_sender.as_ref().unwrap(), bb, &bank, &batch, &mut mint_decimals, Some(tx_results), Some(commited_first_transaction_index)).is_none());
                    }
                }

                drop(batch);

                //ee.reindex_with_address_book();
                processed_ee_sender.send(solana_scheduler::UnlockablePayload(ee, timings)).unwrap();
                break;
                }
                },
                solana_scheduler::ExecutablePayload(solana_scheduler::Flushable::Flush) => {
                    checkpoint.wait_for_restart_from_internal_thread(latest_scheduler_context.take());
                }
                }
            }
            todo!();

            Ok((started.0.elapsed(), started.1.elapsed()))
        }).unwrap()}).collect();

        let collected_results = Arc::new(std::sync::Mutex::new(None));
        let collected_results_in_collector_thread = Arc::clone(&collected_results);

        let error_collector_thread_handle = std::thread::Builder::new()
            .name(format!("solScErrCol{:02}", 0))
            .spawn({
                let checkpoint = checkpoint.clone();

                move || {
                let started = (cpu_time::ThreadTime::now(), std::time::Instant::now());
                if max_thread_priority {
                    thread_priority::set_current_thread_priority(
                        thread_priority::ThreadPriority::Max,
                    )
                    .unwrap();
                }

                let mut cumulative_timings = ExecuteTimings::default();
                use variant_counter::VariantCount;
                let mut transaction_error_counts = TransactionError::counter();
                let (mut skipped, mut succeeded) = (0, 0);
                let mut latest_scheduler_context = None;

                loop {
                while let Ok(r) = retired_ee_receiver.recv_timeout(std::time::Duration::from_millis(20))
                {
                    use solana_runtime::transaction_priority_details::GetTransactionPriorityDetails;
                    if latest_scheduler_context.is_none() {
                        latest_scheduler_context = checkpoint.use_context_value();
                    }

                    match r {
                        solana_scheduler::ExaminablePayload(solana_scheduler::Flushable::Payload((mut ee, timings))) => {
                            cumulative_timings.accumulate(&timings);

                            if let Some(result) = ee.execution_result.take() {
                                match result {
                                    Ok(_) => {
                                        succeeded += 1;
                                        inc_new_counter_info!("bank-process_transactions", 1);
                                        inc_new_counter_info!(
                                            "bank-process_transactions-txs",
                                            1 as usize
                                        );
                                        inc_new_counter_info!("bank-process_transactions-sigs", ee.task.tx.0.signatures().len() as usize);
                                    },
                                    Err(e) => {
                                        transaction_error_counts.record(&e);
                                        match latest_scheduler_context.as_ref().unwrap().mode {
                                            solana_scheduler::Mode::Replaying => {
                                                error!(
                                                    "scheduler: Unexpected validator error: {:?}, transaction: {:?}",
                                                    e, ee.task.tx.0
                                                );
                                            }
                                        };
                                        collected_results_in_collector_thread
                                            .lock()
                                            .unwrap()
                                            .push(Err(e));
                                    }
                                }
                            } else {
                                skipped += 1;
                            }
                            drop(ee);
                        },
                        solana_scheduler::ExaminablePayload(solana_scheduler::Flushable::Flush) => {
                            info!("post_execution_handler: {} {:?}", SchedulerContext::log_prefix(random_id, latest_scheduler_context.as_ref()), transaction_error_counts.aggregate().into_iter().chain([("succeeded", succeeded), ("skipped", skipped)].into_iter()).filter(|&(k, v)| v > 0).collect::<std::collections::BTreeMap<_, _>>());
                            if let Some(solana_scheduler::Mode::Replaying) = latest_scheduler_context.as_ref().map(|c| c.mode) {
                                assert_eq!(skipped, 0);
                            }
                            transaction_error_counts.reset();
                            (succeeded, skipped) = (0, 0);
                            let propagate_tx_error = match latest_scheduler_context.as_ref().unwrap().mode {
                                solana_scheduler::Mode::Replaying => true, 
                            };
                            if !propagate_tx_error {
                                collected_results_in_collector_thread
                                    .lock()
                                    .unwrap()
                                    .clear()
                            }
                            checkpoint.register_return_value(std::mem::take(&mut cumulative_timings));
                            checkpoint.wait_for_restart_from_internal_thread(latest_scheduler_context.take());
                        },
                    }
                }
                }
                todo!();

                Ok((started.0.elapsed(), started.1.elapsed()))
            }})
            .unwrap();


        let scheduler_thread_handle = std::thread::Builder::new()
            .name("solScheduler".to_string())
            .spawn({
                let checkpoint = checkpoint.clone();

                move || {
                let started = (cpu_time::ThreadTime::now(), std::time::Instant::now());
                if max_thread_priority {
                    thread_priority::set_current_thread_priority(
                        thread_priority::ThreadPriority::Max,
                    )
                    .unwrap();
                }

                loop {
                    let mut runnable_queue = solana_scheduler::TaskQueue::default();
                    let mut scheduler_context = solana_scheduler::ScheduleStage::run(
                        &checkpoint,
                        executing_thread_count,
                        &mut runnable_queue,
                        &mut address_book,
                        &transaction_receiver,
                        &scheduled_ee_sender,
                        Some(&scheduled_high_ee_sender),
                        &processed_ee_receiver,
                        Some(&retired_ee_sender),
                        |context| SchedulerContext::log_prefix(random_id, context.as_ref()),
                    );
                    if scheduler_context.is_none() {
                       scheduler_context = checkpoint.use_context_value();
                    }
                    checkpoint.wait_for_restart_from_internal_thread(scheduler_context);
                    continue;
                }

                drop(transaction_receiver);
                drop(scheduled_ee_sender);
                drop(scheduled_high_ee_sender);
                drop(processed_ee_receiver);

                todo!();
                Ok((started.0.elapsed(), started.1.elapsed()))
            }})
            .unwrap();

        let s = Self {
            random_id,
            scheduler_thread_handle: Some(scheduler_thread_handle),
            executing_thread_handles: Some(executing_thread_handles),
            error_collector_thread_handle: Some(error_collector_thread_handle),
            transaction_sender: Some(transaction_sender),
            preloader,
            graceful_stop_initiated: Default::default(),
            collected_results,
            commit_status,
            checkpoint,
            stopped_mode: Default::default(),
            current_scheduler_context: Default::default(),
            thread_count,
            scheduler_pool,
        };
        info!(
            "scheduler: id_{:016x} setup done with {}us",
            random_id,
            start.elapsed().as_micros()
        );

        s
    }
}

impl Scheduler {
    fn new_checkpoint(thread_count: usize) -> Arc<Checkpoint> {
        Checkpoint::new(thread_count)
    }

    fn replace_scheduler_context_inner(&self, context: SchedulerContext) {
        self.checkpoint.replace_context_value(context);
    }

    fn clear_current_scheduler_context_inner(&self) {
        drop(self.current_scheduler_context.write().unwrap().take());
    }

    fn scheduler_context_inner(&self) -> Option<SchedulerContext> {
        let mut sc = self.current_scheduler_context.write().unwrap();
        if let Some(sc) = &mut *sc {
            Some(sc.clone())
        } else {
            assert!(self.stopped_mode.is_none());
            let ssc = self.checkpoint.use_context_value();
            assert!(ssc.is_some());
            *sc = ssc;
            sc.clone()
        }
    }

    fn do_trigger_stop(&mut self, is_restart: bool) {
        if self.graceful_stop_initiated {
            return;
        }
        self.graceful_stop_initiated = true;

        info!(
            "Scheduler::trigger_stop(): {} triggering stop..",
            SchedulerContext::log_prefix(self.random_id, self.scheduler_context().as_ref()),
        );
        //let transaction_sender = self.transaction_sender.take().unwrap();

        //drop(transaction_sender);
        self.transaction_sender
            .as_ref()
            .unwrap()
            .send(solana_scheduler::SchedulablePayload(
                solana_scheduler::Flushable::Flush,
            ))
            .unwrap();

        if !is_restart {
            self.stopped_mode = Some(self.current_scheduler_mode());
            self.clear_current_scheduler_context_inner();
        }
    }

    fn do_clear_stop(&mut self, is_restart: bool) {
        assert!(self.graceful_stop_initiated);
        self.graceful_stop_initiated = false;
        if is_restart {
            assert_eq!(
                self.stopped_mode.is_none(),
                true,
            );
        } else {
            assert!(self.collected_results().lock().unwrap().is_none());
            drop(self.stopped_mode.take().unwrap());
            assert!(self.current_scheduler_context.write().unwrap().is_none());
        }
        self.checkpoint.wait_for_completed_restart();
        if is_restart {
            self.checkpoint.replace_context_value(self.current_scheduler_context.write().unwrap().take().unwrap());
        }
    }

    fn scheduler_context(&self) -> Option<SchedulerContext> {
        self.scheduler_context_inner()
    }

    fn collected_results(&self) -> Arc<std::sync::Mutex<Vec<Result<ExecuteTimings>>>> {
        self.collected_results.clone()
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        let current_thread_name = std::thread::current().name().unwrap().to_string();
        warn!("Scheduler::drop() by {}...", current_thread_name);
        todo!();
        //info!("Scheduler::drop(): id_{:016x} begin..", self.random_id);
        //self.gracefully_stop().unwrap();
        //info!("Scheduler::drop(): id_{:016x} end...", self.random_id);
    }
}


impl LikeScheduler for Scheduler {
    fn random_id(&self) -> u64 {
        self.random_id
    }

    fn schedule_execution(&self, sanitized_tx: &SanitizedTransaction, index: usize, mode: solana_scheduler::Mode) {
        trace!("Scheduler::schedule()");
        #[derive(Clone, Copy, Debug)]
        struct NotAtTopOfScheduleThread;
        unsafe impl solana_scheduler::NotAtScheduleThread for NotAtTopOfScheduleThread {}
        let nast = NotAtTopOfScheduleThread;

        let locks = sanitized_tx.get_account_locks_unchecked();
        let writable_lock_iter = locks.writable.iter().map(|address| {
            solana_scheduler::LockAttempt::new(
                self.preloader.load(**address),
                solana_scheduler::RequestedUsage::Writable,
            )
        });
        let readonly_lock_iter = locks.readonly.iter().map(|address| {
            solana_scheduler::LockAttempt::new(
                self.preloader.load(**address),
                solana_scheduler::RequestedUsage::Readonly,
            )
        });
        let locks = writable_lock_iter
            .chain(readonly_lock_iter)
            .collect::<Vec<_>>();

        //assert_eq!(index, self.transaction_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        use solana_scheduler::{Mode, UniqueWeight};
        use solana_runtime::transaction_priority_details::GetTransactionPriorityDetails;
        let uw = match mode {
            Mode::Replaying => solana_scheduler::UniqueWeight::max_value() - index as solana_scheduler::UniqueWeight,
        };
        let t =
            solana_scheduler::Task::new_for_queue(nast, uw, (sanitized_tx.clone(), locks));
        self.transaction_sender
            .as_ref()
            .unwrap()
            .send(solana_scheduler::SchedulablePayload(
                solana_scheduler::Flushable::Payload(t),
            ))
            .unwrap();
    }

    fn handle_aborted_executions(&self) -> (ExecuteTimings, Result<()>) {
        std::mem::take(&mut self.collected_results.lock().unwrap())
    }

    fn gracefully_stop(&mut self, from_internal: bool, is_restart: bool) -> Result<()> {
        self.do_trigger_stop(is_restart);
        let label = format!("id_{:016x}", self.random_id); //SchedulerContext::log_prefix(self.random_id, self.scheduler_context().as_ref());
        info!(
            "Scheduler::gracefully_stop(): {} {} waiting.. from_internal: {from_internal} is_restart: {is_restart}", label, std::thread::current().name().unwrap().to_string()
        );

        info!("just before wait for restart...");
        if from_internal {
            self.checkpoint.ignore_external_thread();
        }
        self.checkpoint.wait_for_restart();
        self.collected_results.lock().unwrap() = Some(self.checkpoint.take_restart_value());

        /*
        let executing_thread_duration_pairs: Result<Vec<_>> = self.executing_thread_handles.take().unwrap().into_iter().map(|executing_thread_handle| {
            executing_thread_handle.join().unwrap().map(|u| (u.0.as_micros(), u.1.as_micros()))
        }).collect();
        let mut executing_thread_duration_pairs = executing_thread_duration_pairs?;
        executing_thread_duration_pairs.sort();
        let (executing_thread_cpu_us, executing_thread_wall_time_us): (Vec<_>, Vec<_>) = executing_thread_duration_pairs.into_iter().unzip();

        let h = self.scheduler_thread_handle.take().unwrap();
        let scheduler_thread_duration_pairs = h.join().unwrap()?;
        let (scheduler_thread_cpu_us, scheduler_thread_wall_time_us) = (scheduler_thread_duration_pairs.0.as_micros(), scheduler_thread_duration_pairs.1.as_micros());
        let h = self.error_collector_thread_handle.take().unwrap();
        let error_collector_thread_duration_pairs = h.join().unwrap()?;
        let (error_collector_thread_cpu_us, error_collector_thread_wall_time_us) = (error_collector_thread_duration_pairs.0.as_micros(), error_collector_thread_duration_pairs.1.as_micros());

        info!("Scheduler::gracefully_stop(): slot: {} id_{:016x} durations 1/2 (cpu ): scheduler: {}us, error_collector: {}us, lanes: {}us = {:?}", self.slot.map(|s| format!("{}", s)).unwrap_or("-".into()), self.random_id, scheduler_thread_cpu_us, error_collector_thread_cpu_us, executing_thread_cpu_us.iter().sum::<u128>(), &executing_thread_cpu_us);
        info!("Scheduler::gracefully_stop(): slot: {} id_{:016x} durations 2/2 (wall): scheduler: {}us, error_collector: {}us, lanes: {}us = {:?}", self.slot.map(|s| format!("{}", s)).unwrap_or("-".into()), self.random_id, scheduler_thread_wall_time_us, error_collector_thread_wall_time_us, executing_thread_wall_time_us.iter().sum::<u128>(), &executing_thread_wall_time_us);
        */

        if is_restart {
            self.do_clear_stop(true);
        }

        info!(
            "Scheduler::gracefully_stop(): {} waiting done.. from_internal: {from_internal} is_restart: {is_restart}", label,
        );
        Ok(())
    }

    fn clear_stop(&mut self) {
        self.do_clear_stop(false);
    }

    fn trigger_stop(&mut self) {
        self.do_trigger_stop(false);
    }

    fn current_scheduler_mode(&self) -> solana_scheduler::Mode {
        self.stopped_mode.unwrap_or_else(||
            self.scheduler_context().unwrap().mode
        )
    }

    fn scheduler_pool(&self) -> Box<dyn LikeSchedulerPool> {
        Box::new(SchedulerPoolWrapper(self.scheduler_pool.clone()))
    }

    fn replace_scheduler_context(&self, context: SchedulerContext) {
        self.replace_scheduler_context_inner(context);
    }
}

fn send_transaction_status(sender: &TransactionStatusSender, pre: Option<(Vec<Vec<u64>>, Vec<Vec<TransactionTokenBalance>>)>, bank: &Arc<Bank>, batch: &TransactionBatch, mut mint_decimals: &mut HashMap<Pubkey, u8>, tx_results: Option<TransactionResults>, commited_first_transaction_index: Option<usize>) -> std::option::Option<(Vec<Vec<u64>>, Vec<Vec<TransactionTokenBalance>>)> {
    match pre {
        None => {
            Some((
                bank.collect_balances(batch),
                collect_token_balances(bank, batch, mint_decimals),
            ))
        },
        Some((pre_native_balances, pre_token_balances)) => {
            let tx_results = tx_results.unwrap();
            let commited_first_transaction_index = commited_first_transaction_index.unwrap();
            let TransactionResults {
                fee_collection_results,
                execution_results,
                rent_debits,
                ..
            } = tx_results;

            let post_native_balances = bank.collect_balances(batch);
            let post_token_balances = collect_token_balances(bank, batch, mint_decimals);
            let token_balances =
                TransactionTokenBalancesSet::new(pre_token_balances, post_token_balances);

            sender.send_transaction_status_batch(
                bank.clone(),
                batch.sanitized_transactions().to_vec(),
                execution_results,
                TransactionBalancesSet::new(
                    pre_native_balances,
                    post_native_balances,
                ),
                token_balances,
                rent_debits,
                vec![commited_first_transaction_index],
            );
            None
        }
    }
}
