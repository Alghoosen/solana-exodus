use {
    crate::rpc_subscriptions::RpcSubscriptions,
    crossbeam_channel::RecvTimeoutError,
    solana_client::rpc_response::SlotUpdate,
    solana_ledger::blockstore::{CompletedSlotsReceiver, OptimisticDistributionSlotReceiver},
    solana_sdk::timing::timestamp,
    std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread::{Builder, JoinHandle},
        time::Duration,
    },
};

const SLOT_SERVICE_RECEIVE_TIMEOUT_MS: u64 = 100;

pub struct RpcCompletedSlotsService;
impl RpcCompletedSlotsService {
    pub fn spawn(
        completed_slots_receiver: CompletedSlotsReceiver,
        rpc_subscriptions: Arc<RpcSubscriptions>,
        exit: Arc<AtomicBool>,
    ) -> JoinHandle<()> {
        Builder::new()
            .name("solana-rpc-completed-slots-service".to_string())
            .spawn(move || loop {
                // received exit signal, shutdown the service
                if exit.load(Ordering::Relaxed) {
                    break;
                }

                match completed_slots_receiver
                    .recv_timeout(Duration::from_millis(SLOT_SERVICE_RECEIVE_TIMEOUT_MS))
                {
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        info!("RpcCompletedSlotsService channel disconnected, exiting.");
                        break;
                    }
                    Ok(slots) => {
                        for slot in slots {
                            rpc_subscriptions.notify_slot_update(SlotUpdate::Completed {
                                slot,
                                timestamp: timestamp(),
                            });
                        }
                    }
                }
            })
            .unwrap()
    }
}

pub struct RpcOptimisticDistributionSlotService;
impl RpcOptimisticDistributionSlotService {
    pub fn spawn(
        optimistic_distribution_slot_receiver: OptimisticDistributionSlotReceiver,
        rpc_subscriptions: Arc<RpcSubscriptions>,
        exit: Arc<AtomicBool>,
    ) -> JoinHandle<()> {
        Builder::new()
            .name("solana-rpc-optimistic-distribution-slot-service".to_string())
            .spawn(move || loop {
                // received exit signal, shutdown the service
                if exit.load(Ordering::Relaxed) {
                    break;
                }
                match optimistic_distribution_slot_receiver
                    .recv_timeout(Duration::from_millis(SLOT_SERVICE_RECEIVE_TIMEOUT_MS))
                {
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        info!(
                            "RpcOptimisticDistributionSlotService channel disconnected, exiting."
                        );
                        break;
                    }
                    Ok(slot) => {
                        rpc_subscriptions.notify_slot_update(SlotUpdate::OptimisticDistribution {
                            slot,
                            timestamp: timestamp(),
                        });
                    }
                }
            })
            .unwrap()
    }
}
