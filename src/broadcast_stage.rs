//! The `broadcast_stage` broadcasts data from a leader node to validators
//!
use counter::Counter;
use crdt::{Crdt, CrdtError, NodeInfo};
use entry::Entry;
#[cfg(feature = "erasure")]
use erasure;
use leader_scheduler::LeaderScheduler;
use ledger::Block;
use log::Level;
use packet::SharedBlobs;
use rayon::prelude::*;
use result::{Error, Result};
use service::Service;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, RwLock};
use std::thread::{self, Builder, JoinHandle};
use std::time::{Duration, Instant};
use timing::duration_as_ms;
use window::{self, SharedWindow, WindowIndex, WindowUtil, WINDOW_SIZE};

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum BroadcastStageReturnType {
    LeaderRotation,
    ChannelDisconnected,
}

fn broadcast(
    leader_rotation_interval_option: &Option<u64>,
    leader_scheduler_option: &Option<Arc<RwLock<LeaderScheduler>>>,
    node_info: &NodeInfo,
    broadcast_table: &[NodeInfo],
    window: &SharedWindow,
    receiver: &Receiver<Vec<Entry>>,
    sock: &UdpSocket,
    transmit_index: &mut WindowIndex,
    receive_index: &mut u64,
) -> Result<()> {
    let id = node_info.id;
    let timer = Duration::new(1, 0);
    let entries = receiver.recv_timeout(timer)?;
    let now = Instant::now();
    let mut num_entries = entries.len();
    let mut ventries = Vec::new();
    ventries.push(entries);
    while let Ok(entries) = receiver.try_recv() {
        num_entries += entries.len();
        ventries.push(entries);
    }
    inc_new_counter_info!("broadcast_stage-entries_received", num_entries);

    let to_blobs_start = Instant::now();
    let dq: SharedBlobs = ventries
        .into_par_iter()
        .flat_map(|p| p.to_blobs())
        .collect();

    let to_blobs_elapsed = duration_as_ms(&to_blobs_start.elapsed());

    // flatten deque to vec
    let blobs_vec: SharedBlobs = dq.into_iter().collect();

    let blobs_chunking = Instant::now();
    // We could receive more blobs than window slots so
    // break them up into window-sized chunks to process
    let blobs_chunked = blobs_vec.chunks(WINDOW_SIZE as usize).map(|x| x.to_vec());
    let chunking_elapsed = duration_as_ms(&blobs_chunking.elapsed());

    trace!("{}", window.read().unwrap().print(&id, *receive_index));

    let broadcast_start = Instant::now();
    for mut blobs in blobs_chunked {
        let blobs_len = blobs.len();
        trace!("{}: broadcast blobs.len: {}", id, blobs_len);

        // Index the blobs
        window::index_blobs(node_info, &blobs, receive_index)
            .expect("index blobs for initial window");

        // keep the cache of blobs that are broadcast
        inc_new_counter_info!("streamer-broadcast-sent", blobs.len());
        {
            let mut win = window.write().unwrap();
            assert!(blobs.len() <= win.len());
            for b in &blobs {
                let ix = b.read().unwrap().get_index().expect("blob index");
                let pos = (ix % WINDOW_SIZE) as usize;
                if let Some(x) = win[pos].data.take() {
                    trace!(
                        "{} popped {} at {}",
                        id,
                        x.read().unwrap().get_index().unwrap(),
                        pos
                    );
                }
                if let Some(x) = win[pos].coding.take() {
                    trace!(
                        "{} popped {} at {}",
                        id,
                        x.read().unwrap().get_index().unwrap(),
                        pos
                    );
                }

                trace!("{} null {}", id, pos);
            }
            for b in &blobs {
                let ix = b.read().unwrap().get_index().expect("blob index");
                let pos = (ix % WINDOW_SIZE) as usize;
                trace!("{} caching {} at {}", id, ix, pos);
                assert!(win[pos].data.is_none());
                win[pos].data = Some(b.clone());
            }
        }

        // Fill in the coding blob data from the window data blobs
        #[cfg(feature = "erasure")]
        {
            erasure::generate_coding(
                &id,
                &mut window.write().unwrap(),
                *receive_index,
                blobs_len,
                &mut transmit_index.coding,
            )?;
        }

        *receive_index += blobs_len as u64;

        // Send blobs out from the window
        Crdt::broadcast(
            leader_rotation_interval_option,
            leader_scheduler_option,
            &node_info,
            &broadcast_table,
            &window,
            &sock,
            transmit_index,
            *receive_index,
        )?;
    }
    let broadcast_elapsed = duration_as_ms(&broadcast_start.elapsed());

    inc_new_counter_info!(
        "broadcast_stage-time_ms",
        duration_as_ms(&now.elapsed()) as usize
    );
    info!(
        "broadcast: {} entries, blob time {} chunking time {} broadcast time {}",
        num_entries, to_blobs_elapsed, chunking_elapsed, broadcast_elapsed
    );

    Ok(())
}

// Implement a destructor for the BroadcastStage thread to signal it exited
// even on panics
struct Finalizer {
    exit_sender: Arc<AtomicBool>,
}

impl Finalizer {
    fn new(exit_sender: Arc<AtomicBool>) -> Self {
        Finalizer { exit_sender }
    }
}
// Implement a destructor for Finalizer.
impl Drop for Finalizer {
    fn drop(&mut self) {
        self.exit_sender.clone().store(true, Ordering::Relaxed);
    }
}

pub struct BroadcastStage {
    thread_hdl: JoinHandle<BroadcastStageReturnType>,
}

impl BroadcastStage {
    fn run(
        sock: &UdpSocket,
        crdt: &Arc<RwLock<Crdt>>,
        window: &SharedWindow,
        entry_height: u64,
        receiver: &Receiver<Vec<Entry>>,
        leader_scheduler_option: Option<Arc<RwLock<LeaderScheduler>>>,
    ) -> BroadcastStageReturnType {
        let mut transmit_index = WindowIndex {
            data: entry_height,
            coding: entry_height,
        };
        let mut receive_index = entry_height;
        let me = crdt.read().unwrap().my_data().clone();
        let mut leader_rotation_interval_option = None;
        if let Some(ref leader_scheduler) = leader_scheduler_option {
            leader_rotation_interval_option =
                Some(leader_scheduler.read().unwrap().leader_rotation_interval);
        }
        loop {
            let broadcast_table = crdt.read().unwrap().compute_broadcast_table();
            if let Err(e) = broadcast(
                &leader_rotation_interval_option,
                &leader_scheduler_option,
                &me,
                &broadcast_table,
                &window,
                &receiver,
                &sock,
                &mut transmit_index,
                &mut receive_index,
            ) {
                match e {
                    Error::RecvTimeoutError(RecvTimeoutError::Disconnected) => {
                        return BroadcastStageReturnType::ChannelDisconnected
                    }
                    Error::RecvTimeoutError(RecvTimeoutError::Timeout) => (),
                    Error::CrdtError(CrdtError::NoPeers) => (), // TODO: Why are the unit-tests throwing hundreds of these?
                    _ => {
                        inc_new_counter_info!("streamer-broadcaster-error", 1, 1);
                        error!("broadcaster error: {:?}", e);
                    }
                }
            }
        }
    }

    /// Service to broadcast messages from the leader to layer 1 nodes.
    /// See `crdt` for network layer definitions.
    /// # Arguments
    /// * `sock` - Socket to send from.
    /// * `exit` - Boolean to signal system exit.
    /// * `crdt` - CRDT structure
    /// * `window` - Cache of blobs that we have broadcast
    /// * `receiver` - Receive channel for blobs to be retransmitted to all the layer 1 nodes.
    /// * `exit_sender` - Set to true when this stage exits, allows rest of Tpu to exit cleanly. Otherwise,
    /// when a Tpu stage closes, it only closes the stages that come after it. The stages
    /// that come before could be blocked on a receive, and never notice that they need to
    /// exit. Now, if any stage of the Tpu closes, it will lead to closing the WriteStage (b/c
    /// WriteStage is the last stage in the pipeline), which will then close Broadcast stage,
    /// which will then close FetchStage in the Tpu, and then the rest of the Tpu,
    /// completing the cycle.
    pub fn new(
        sock: UdpSocket,
        crdt: Arc<RwLock<Crdt>>,
        window: SharedWindow,
        entry_height: u64,
        receiver: Receiver<Vec<Entry>>,
        leader_scheduler_option: Option<Arc<RwLock<LeaderScheduler>>>,
        exit_sender: Arc<AtomicBool>,
    ) -> Self {
        let thread_hdl = Builder::new()
            .name("solana-broadcaster".to_string())
            .spawn(move || {
                let _exit = Finalizer::new(exit_sender);
                Self::run(
                    &sock,
                    &crdt,
                    &window,
                    entry_height,
                    &receiver,
                    leader_scheduler_option,
                )
            }).unwrap();

        BroadcastStage { thread_hdl }
    }
}

impl Service for BroadcastStage {
    type JoinReturnType = BroadcastStageReturnType;

    fn join(self) -> thread::Result<BroadcastStageReturnType> {
        self.thread_hdl.join()
    }
}
