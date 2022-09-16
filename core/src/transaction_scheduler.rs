//! Interface for a transaction scheduler to be used by the banking stage.
//!

use {
    crate::{
        bank_process_decision::BankPacketProcessingDecision,
        immutable_deserialized_packet::ImmutableDeserializedPacket,
    },
    crossbeam_channel::RecvTimeoutError,
    std::{rc::Rc, time::Duration},
};

pub mod priority_queue_scheduler;

pub trait TransactionSchedulerBankingHandle {
    /// Get the next batch of transactions to process, and the instructions for processing them.
    fn get_next_transaction_batch(
        &mut self,
        timeout: Duration,
    ) -> Result<Rc<ScheduledPacketBatch>, RecvTimeoutError>;

    /// Signal that the current batch of transactions has been processed.
    fn complete_batch(&mut self, batch: ProcessedPacketBatch);
}

/// Message: [Scheduler -> BankingStage]
/// The next batch of transactions to process, with instructions for processing them, and a unique id.
pub struct ScheduledPacketBatch {
    /// Unique identifier for the batch of transactions
    pub id: ScheduledPacketBatchId,
    /// Instruction for processing packets
    pub processing_instruction: BankingProcessingInstruction,
    /// Deserialized packets to process.
    pub deserialized_packets: Vec<Rc<ImmutableDeserializedPacket>>,
}

/// Message: [BankingStage -> Scheduler]
/// Indicates a batch of transactions has been processed, and which transactions
/// need to be retried.
pub struct ProcessedPacketBatch {
    /// Identifier for the batch of transactions - this should always match the id the scheduler expects.
    pub id: ScheduledPacketBatchId,
    /// Transactions that need to be retried, i.e. added back to the scheduler.
    // TODO: This should be a bitset, and go away entirely once we have a better scheduler
    pub retryable_packets: Vec<bool>,
}

#[derive(Clone, Copy, Debug)]
pub enum BankingProcessingInstruction {
    /// Process transactions and attempt to commit them to the bank.
    Consume,
    /// Forward transactions to the leader(s) for processing. This
    /// instructs the bank to forward, but the scheduler may still
    /// hold on to these transactions for a while.
    Forward,
}

impl From<BankPacketProcessingDecision> for BankingProcessingInstruction {
    fn from(decision: BankPacketProcessingDecision) -> Self {
        match decision {
            BankPacketProcessingDecision::Consume(_) => BankingProcessingInstruction::Consume,
            BankPacketProcessingDecision::Forward
            | BankPacketProcessingDecision::ForwardAndHold => BankingProcessingInstruction::Forward,
            BankPacketProcessingDecision::Hold => {
                panic!("Hold decision should not be converted to a bank processing instruction.")
            }
        }
    }
}

/// Unique identifier for a batch of transactions, wrapped in a struct to prevent
/// comparisons of batch ids. There should be no reason to expect any ordering of
/// batch ids generated by a scheduler, other than being unique for outstanding
/// batches.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ScheduledPacketBatchId(u64);

impl ScheduledPacketBatchId {
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

/// Generates a unique identifier for a batch of transactions.
#[derive(Default)]
pub struct ScheduledPacketBatchIdGenerator {
    next_id: u64,
}

impl ScheduledPacketBatchIdGenerator {
    /// Generates a new unique identifier for a batch of transactions.
    pub fn generate_id(&mut self) -> ScheduledPacketBatchId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        ScheduledPacketBatchId::new(id)
    }
}
