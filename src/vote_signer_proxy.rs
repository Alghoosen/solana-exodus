//! The `vote_signer_proxy` votes on the `last_id` of the bank at a regular cadence

use crate::bank::Bank;
use crate::cluster_info::ClusterInfo;
use crate::counter::Counter;
use crate::jsonrpc_core;
use crate::packet::SharedBlob;
use crate::result::{Error, Result};
use crate::rpc_request::{RpcClient, RpcRequest};
use crate::streamer::BlobSender;
use bincode::serialize;
use log::Level;
use solana_sdk::hash::Hash;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, KeypairUtil, Signature};
use solana_sdk::transaction::Transaction;
use solana_sdk::vote_program::Vote;
use solana_sdk::vote_transaction::VoteTransaction;
use solana_vote_signer::rpc::VoteSigner;
use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, RwLock};

#[derive(Debug, PartialEq, Eq)]
pub enum VoteError {
    NoValidSupermajority,
    NoLeader,
    LeaderInfoNotFound,
}

pub struct RemoteVoteSigner {
    rpc_client: RpcClient,
}

impl RemoteVoteSigner {
    pub fn new(signer: SocketAddr) -> Self {
        Self {
            rpc_client: RpcClient::new_from_socket(signer),
        }
    }
}

impl VoteSigner for RemoteVoteSigner {
    fn register(
        &self,
        pubkey: Pubkey,
        sig: &Signature,
        msg: &[u8],
    ) -> jsonrpc_core::Result<Pubkey> {
        let params = json!([pubkey, sig, msg]);
        let resp = self
            .rpc_client
            .retry_make_rpc_request(1, &RpcRequest::RegisterNode, Some(params), 5)
            .unwrap();
        let vote_account: Pubkey = serde_json::from_value(resp).unwrap();
        Ok(vote_account)
    }
    fn sign(&self, pubkey: Pubkey, sig: &Signature, msg: &[u8]) -> jsonrpc_core::Result<Signature> {
        let params = json!([pubkey, sig, msg]);
        let resp = self
            .rpc_client
            .retry_make_rpc_request(1, &RpcRequest::SignVote, Some(params), 0)
            .unwrap();
        let vote_signature: Signature = serde_json::from_value(resp).unwrap();
        Ok(vote_signature)
    }
    fn deregister(&self, pubkey: Pubkey, sig: &Signature, msg: &[u8]) -> jsonrpc_core::Result<()> {
        let params = json!([pubkey, sig, msg]);
        let _resp = self
            .rpc_client
            .retry_make_rpc_request(1, &RpcRequest::DeregisterNode, Some(params), 5)
            .unwrap();
        Ok(())
    }
}

pub struct VoteSignerProxy {
    keypair: Arc<Keypair>,
    signer: Box<VoteSigner + Send + Sync>,
    pub vote_account: Pubkey,
    last_leader: RwLock<Pubkey>,
    unsent_votes: RwLock<Vec<Transaction>>,
}

impl VoteSignerProxy {
    pub fn new(keypair: &Arc<Keypair>, signer: Box<VoteSigner + Send + Sync>) -> Self {
        let msg = "Registering a new node";
        let sig = Signature::new(&keypair.sign(msg.as_bytes()).as_ref());
        let vote_account = signer
            .register(keypair.pubkey(), &sig, msg.as_bytes())
            .unwrap();
        Self {
            keypair: keypair.clone(),
            signer,
            vote_account,
            last_leader: RwLock::new(vote_account),
            unsent_votes: RwLock::new(vec![]),
        }
    }

    pub fn new_vote_account(&self, bank: &Bank, num_tokens: u64, last_id: Hash) -> Result<()> {
        // Create and register the new vote account
        let tx =
            Transaction::vote_account_new(&self.keypair, self.vote_account, last_id, num_tokens, 0);
        bank.process_transaction(&tx)?;
        Ok(())
    }

    pub fn send_validator_vote(
        &self,
        bank: &Arc<Bank>,
        cluster_info: &Arc<RwLock<ClusterInfo>>,
        vote_blob_sender: &BlobSender,
    ) -> Result<()> {
        {
            let (leader, _) = bank
                .get_current_leader()
                .expect("Scheduled leader should be calculated by this point");

            let mut old_leader = self.last_leader.write().unwrap();

            if leader != *old_leader {
                *old_leader = leader;
                self.unsent_votes.write().unwrap().clear();
            }
            inc_new_counter_info!(
                "validator-total_pending_votes",
                self.unsent_votes.read().unwrap().len()
            );
        }

        let tx = self.new_signed_vote_transaction(&bank.last_id(), bank.tick_height());

        match VoteSignerProxy::get_leader_tpu(&bank, cluster_info) {
            Ok(tpu) => {
                self.unsent_votes.write().unwrap().retain(|old_tx| {
                    if let Ok(shared_blob) = self.new_signed_vote_blob(old_tx.clone(), tpu) {
                        inc_new_counter_info!("validator-pending_vote_sent", 1);
                        inc_new_counter_info!("validator-vote_sent", 1);
                        vote_blob_sender.send(vec![shared_blob]).unwrap();
                    }
                    false
                });
                if let Ok(shared_blob) = self.new_signed_vote_blob(tx, tpu) {
                    inc_new_counter_info!("validator-vote_sent", 1);
                    vote_blob_sender.send(vec![shared_blob])?;
                }
            }
            Err(e) => {
                self.unsent_votes.write().unwrap().push(tx);
                inc_new_counter_info!("validator-new_pending_vote", 1);
                return Err(e);
            }
        };

        Ok(())
    }

    pub fn new_signed_vote_transaction(&self, last_id: &Hash, tick_height: u64) -> Transaction {
        let vote = Vote { tick_height };
        let tx = Transaction::vote_new(&self.vote_account, vote, *last_id, 0);

        let msg = tx.get_sign_data();
        let sig = Signature::new(&self.keypair.sign(&msg).as_ref());

        let keypair = self.keypair.clone();
        let vote_signature = self.signer.sign(keypair.pubkey(), &sig, &msg).unwrap();
        Transaction {
            signatures: vec![vote_signature],
            account_keys: tx.account_keys,
            last_id: tx.last_id,
            fee: tx.fee,
            program_ids: tx.program_ids,
            instructions: tx.instructions,
        }
    }

    fn new_signed_vote_blob(&self, tx: Transaction, leader_tpu: SocketAddr) -> Result<SharedBlob> {
        let shared_blob = SharedBlob::default();
        {
            let mut blob = shared_blob.write().unwrap();
            let bytes = serialize(&tx)?;
            let len = bytes.len();
            blob.data[..len].copy_from_slice(&bytes);
            blob.meta.set_addr(&leader_tpu);
            blob.meta.size = len;
        };

        Ok(shared_blob)
    }

    fn get_leader_tpu(bank: &Bank, cluster_info: &Arc<RwLock<ClusterInfo>>) -> Result<SocketAddr> {
        let leader_id = match bank.get_current_leader() {
            Some((leader_id, _)) => leader_id,
            None => return Err(Error::VoteError(VoteError::NoLeader)),
        };

        let rcluster_info = cluster_info.read().unwrap();
        let leader_tpu = rcluster_info.lookup(leader_id).map(|leader| leader.tpu);
        if let Some(leader_tpu) = leader_tpu {
            Ok(leader_tpu)
        } else {
            Err(Error::VoteError(VoteError::LeaderInfoNotFound))
        }
    }
}

#[cfg(test)]
mod test {
    use crate::bank::Bank;
    use crate::cluster_info::{ClusterInfo, Node};
    use crate::mint::Mint;
    use crate::vote_signer_proxy::VoteSignerProxy;
    use solana_sdk::signature::{Keypair, KeypairUtil};
    use solana_vote_signer::rpc::LocalVoteSigner;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, RwLock};
    use std::time::Duration;

    #[test]
    pub fn test_pending_votes() {
        solana_logger::setup();

        let signer = VoteSignerProxy::new(
            &Arc::new(Keypair::new()),
            Box::new(LocalVoteSigner::default()),
        );

        // Set up dummy node to host a ReplayStage
        let my_keypair = Keypair::new();
        let my_id = my_keypair.pubkey();
        let my_node = Node::new_localhost_with_pubkey(my_id);
        let cluster_info = Arc::new(RwLock::new(ClusterInfo::new(my_node.info.clone())));

        let mint = Mint::new_with_leader(10000, my_id, 500);
        let bank = Arc::new(Bank::new(&mint));
        let (sender, receiver) = channel();

        assert_eq!(signer.unsent_votes.read().unwrap().len(), 0);
        assert!(signer
            .send_validator_vote(&bank, &cluster_info, &sender)
            .is_err());
        assert_eq!(signer.unsent_votes.read().unwrap().len(), 1);
        assert!(receiver.recv_timeout(Duration::from_millis(400)).is_err());

        assert!(signer
            .send_validator_vote(&bank, &cluster_info, &sender)
            .is_err());
        assert_eq!(signer.unsent_votes.read().unwrap().len(), 2);
        assert!(receiver.recv_timeout(Duration::from_millis(400)).is_err());

        bank.leader_scheduler
            .write()
            .unwrap()
            .use_only_bootstrap_leader = true;
        bank.leader_scheduler.write().unwrap().bootstrap_leader = my_id;
        assert!(signer
            .send_validator_vote(&bank, &cluster_info, &sender)
            .is_ok());
        assert!(receiver.recv_timeout(Duration::from_millis(400)).is_ok());

        assert_eq!(signer.unsent_votes.read().unwrap().len(), 0);
    }
}
