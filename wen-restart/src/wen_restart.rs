//! The `wen-restart` module handles automatic repair during a cluster restart

use {
    crate::{
        last_voted_fork_slots_aggregate::LastVotedForkSlotsAggregate,
        solana::wen_restart_proto::{
            LastVotedForkSlotsAggregateRecord, LastVotedForkSlotsRecord, State as RestartState,
            WenRestartProgress,
        },
    },
    log::*,
    prost::Message,
    solana_gossip::{
        cluster_info::{ClusterInfo, GOSSIP_SLEEP_MILLIS},
        epoch_slots::MAX_SLOTS_PER_ENTRY,
    },
    solana_ledger::{ancestor_iterator::AncestorIterator, blockstore::Blockstore},
    solana_program::{clock::Slot, hash::Hash},
    solana_runtime::bank_forks::BankForks,
    solana_sdk::timing::timestamp,
    solana_vote_program::vote_state::VoteTransaction,
    std::{
        collections::{HashMap, HashSet},
        fs::{read, File},
        io::{Cursor, Error, Write},
        path::PathBuf,
        str::FromStr,
        sync::{Arc, RwLock},
        thread::sleep,
        time::Duration,
    },
};

fn send_restart_last_voted_fork_slots(
    last_vote: VoteTransaction,
    blockstore: Arc<Blockstore>,
    cluster_info: Arc<ClusterInfo>,
    progress: &mut WenRestartProgress,
) -> Result<(), Box<dyn std::error::Error>> {
    let last_vote_fork_slots;
    let last_vote_hash;
    match &progress.my_last_voted_fork_slots {
        Some(my_last_voted_fork_slots) => {
            last_vote_fork_slots = my_last_voted_fork_slots.last_vote_fork_slots.clone();
            last_vote_hash = Hash::from_str(&my_last_voted_fork_slots.last_vote_bankhash).unwrap();
        }
        None => {
            // repair and restart option does not work without last voted slot.
            let last_vote_slot = last_vote
                .last_voted_slot()
                .expect("wen_restart doesn't work if local tower is wiped");
            last_vote_hash = last_vote.hash();
            last_vote_fork_slots = AncestorIterator::new_inclusive(last_vote_slot, &blockstore)
                .take(MAX_SLOTS_PER_ENTRY)
                .collect();
        }
    }
    info!(
        "wen_restart last voted fork {} {:?}",
        last_vote_hash, last_vote_fork_slots
    );
    cluster_info.push_restart_last_voted_fork_slots(&last_vote_fork_slots, last_vote_hash);
    progress.set_state(RestartState::LastVotedForkSlots);
    progress.my_last_voted_fork_slots = Some(LastVotedForkSlotsRecord {
        last_vote_fork_slots,
        last_vote_bankhash: last_vote.hash().to_string(),
        shred_version: cluster_info.my_shred_version() as u32,
    });
    Ok(())
}

fn aggregate_restart_last_voted_fork_slots(
    wen_restart_path: &PathBuf,
    cluster_info: Arc<ClusterInfo>,
    bank_forks: Arc<RwLock<BankForks>>,
    slots_to_repair_for_wen_restart: Arc<RwLock<Vec<Slot>>>,
    progress: &mut WenRestartProgress,
) -> Result<(), Box<dyn std::error::Error>> {
    let root_bank;
    {
        root_bank = bank_forks.read().unwrap().root_bank().clone();
    }
    let root_slot = root_bank.slot();
    let mut last_voted_fork_slots_aggregate = LastVotedForkSlotsAggregate::new(
        root_slot,
        0.42,
        root_bank.epoch_stakes(root_bank.epoch()).unwrap(),
        &progress
            .my_last_voted_fork_slots
            .as_ref()
            .unwrap()
            .last_vote_fork_slots,
        &cluster_info.id(),
    );
    let mut cursor = solana_gossip::crds::Cursor::default();
    let mut is_full_slots = HashSet::new();
    progress.last_voted_fork_slots_aggregate = Some(LastVotedForkSlotsAggregateRecord {
        received: HashMap::new(),
    });
    loop {
        let start = timestamp();
        let new_last_voted_fork_slots = cluster_info.get_restart_last_voted_fork_slots(&mut cursor);
        let result = last_voted_fork_slots_aggregate.aggregate(
            new_last_voted_fork_slots,
            progress.last_voted_fork_slots_aggregate.as_mut().unwrap(),
        );
        let mut filtered_slots: Vec<Slot>;
        {
            let my_bank_forks = bank_forks.read().unwrap();
            filtered_slots = result
                .slots_to_repair
                .into_iter()
                .filter(|slot| {
                    if slot <= &root_slot || is_full_slots.contains(slot) {
                        return false;
                    }
                    let is_full = my_bank_forks
                        .get(*slot)
                        .map_or(false, |bank| bank.is_frozen());
                    if is_full {
                        is_full_slots.insert(*slot);
                    }
                    !is_full
                })
                .collect();
        }
        filtered_slots.sort();
        info!(
            "Active peers: {} Slots to repair: {:?}",
            result.active_percenage, &filtered_slots
        );
        if filtered_slots.is_empty() && result.active_percenage > 0.80 {
            *slots_to_repair_for_wen_restart.write().unwrap() = vec![];
            break;
        }
        {
            *slots_to_repair_for_wen_restart.write().unwrap() = filtered_slots;
        }
        write_wen_restart_records(wen_restart_path, progress)?;
        let elapsed = timestamp().saturating_sub(start);
        let time_left = GOSSIP_SLEEP_MILLIS.saturating_sub(elapsed);
        if time_left > 0 {
            sleep(Duration::from_millis(time_left));
        }
    }
    progress.set_state(RestartState::HeaviestFork);
    Ok(())
}

pub fn wait_for_wen_restart(
    wen_restart_path: &PathBuf,
    last_vote: VoteTransaction,
    blockstore: Arc<Blockstore>,
    cluster_info: Arc<ClusterInfo>,
    bank_forks: Arc<RwLock<BankForks>>,
    slots_to_repair_for_wen_restart: Option<Arc<RwLock<Vec<Slot>>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut progress = read_wen_restart_records(wen_restart_path)?;
    while progress.state() != RestartState::Done {
        match progress.state() {
            RestartState::Init => send_restart_last_voted_fork_slots(
                last_vote.clone(),
                blockstore.clone(),
                cluster_info.clone(),
                &mut progress,
            )?,
            RestartState::LastVotedForkSlots => aggregate_restart_last_voted_fork_slots(
                wen_restart_path,
                cluster_info.clone(),
                bank_forks.clone(),
                slots_to_repair_for_wen_restart.clone().unwrap(),
                &mut progress,
            )?,
            // Place holder to make the code compile and run for now.
            _ => progress.set_state(RestartState::Done),
        }
        write_wen_restart_records(wen_restart_path, &progress)?;
    }
    Ok(())
}

fn read_wen_restart_records(records_path: &PathBuf) -> Result<WenRestartProgress, Error> {
    match read(records_path) {
        Ok(buffer) => {
            let progress = WenRestartProgress::decode(&mut Cursor::new(buffer))?;
            info!("read record {:?}", progress);
            Ok(progress)
        }
        Err(e) => {
            warn!("cannot read record {:?}: {:?}", records_path, e);
            Ok(WenRestartProgress {
                state: RestartState::Init.into(),
                my_last_voted_fork_slots: None,
                last_voted_fork_slots_aggregate: None,
            })
        }
    }
}

fn write_wen_restart_records(
    records_path: &PathBuf,
    new_progress: &WenRestartProgress,
) -> Result<(), Error> {
    // overwrite anything if exists
    let mut file = File::create(records_path)?;
    info!("writing new record {:?}", new_progress);
    let mut buf = Vec::with_capacity(new_progress.encoded_len());
    new_progress.encode(&mut buf)?;
    file.write_all(&buf)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        crate::wen_restart::*,
        solana_entry::entry,
        solana_gossip::{
            cluster_info::ClusterInfo,
            contact_info::ContactInfo,
            crds::GossipRoute,
            crds_value::{CrdsData, CrdsValue, RestartLastVotedForkSlots},
            legacy_contact_info::LegacyContactInfo,
        },
        solana_ledger::{blockstore, get_tmp_ledger_path_auto_delete},
        solana_program::{hash::Hash, vote::state::Vote},
        solana_runtime::{
            bank::Bank,
            genesis_utils::{
                create_genesis_config_with_vote_accounts, GenesisConfigInfo, ValidatorVoteKeypairs,
            },
        },
        solana_sdk::{pubkey::Pubkey, signature::Signer, timing::timestamp},
        solana_streamer::socket::SocketAddrSpace,
        std::{fs::read, sync::Arc, thread::Builder},
    };

    #[test]
    fn test_wen_restart_normal_flow() {
        let validator_voting_keypairs: Vec<_> =
            (0..10).map(|_| ValidatorVoteKeypairs::new_rand()).collect();
        let node_keypair = Arc::new(validator_voting_keypairs[0].node_keypair.insecure_clone());
        let shred_version = 2;
        let cluster_info = Arc::new(ClusterInfo::new(
            {
                let mut contact_info =
                    ContactInfo::new_localhost(&node_keypair.pubkey(), timestamp());
                contact_info.set_shred_version(shred_version);
                contact_info
            },
            node_keypair.clone(),
            SocketAddrSpace::Unspecified,
        ));
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let mut wen_restart_proto_path = ledger_path.path().to_path_buf();
        wen_restart_proto_path.push("wen_restart_status.proto");
        let slots_to_repair_for_wen_restart = Some(Arc::new(RwLock::new(Vec::new())));
        let blockstore = Arc::new(blockstore::Blockstore::open(ledger_path.path()).unwrap());
        let GenesisConfigInfo { genesis_config, .. } = create_genesis_config_with_vote_accounts(
            10_000,
            &validator_voting_keypairs,
            vec![100; validator_voting_keypairs.len()],
        );
        let bank = Bank::new_for_tests(&genesis_config);
        let bank_forks = BankForks::new_rw_arc(bank);
        let expected_slots = 400;
        let last_vote_slot = (MAX_SLOTS_PER_ENTRY + expected_slots).try_into().unwrap();
        let last_parent = (MAX_SLOTS_PER_ENTRY >> 1).try_into().unwrap();
        let mut last_vote_fork_slots = Vec::new();
        for i in 0..expected_slots {
            let entries = entry::create_ticks(1, 0, Hash::default());
            let parent_slot = if i > 0 {
                (MAX_SLOTS_PER_ENTRY + i).try_into().unwrap()
            } else {
                last_parent
            };
            let slot = (MAX_SLOTS_PER_ENTRY + i + 1) as Slot;
            let shreds = blockstore::entries_to_test_shreds(
                &entries,
                slot,
                parent_slot,
                false,
                0,
                true, // merkle_variant
            );
            blockstore.insert_shreds(shreds, None, false).unwrap();
            last_vote_fork_slots.push(slot);
        }
        // link directly to slot 1 whose distance to last_vote > MAX_SLOTS_PER_ENTRY so it will not be included.
        let entries = entry::create_ticks(1, 0, Hash::default());
        let shreds = blockstore::entries_to_test_shreds(
            &entries,
            last_parent,
            1,
            false,
            0,
            true, // merkle_variant
        );
        last_vote_fork_slots.extend([last_parent, 1]);
        blockstore.insert_shreds(shreds, None, false).unwrap();
        let last_vote_bankhash = Hash::new_unique();
        let wen_restart_proto_path_clone = wen_restart_proto_path.clone();
        let cluster_info_clone = cluster_info.clone();
        let expected_slots_to_repair: Vec<Slot> =
            (last_vote_slot + 1..last_vote_slot + 3).collect();
        let bank_forks_clone = bank_forks.clone();
        let wen_restart_thread_handle = Builder::new()
            .name("solana-wen-restart".to_string())
            .spawn(move || {
                assert!(wait_for_wen_restart(
                    &wen_restart_proto_path_clone,
                    VoteTransaction::from(Vote::new(vec![last_vote_slot], last_vote_bankhash)),
                    blockstore,
                    cluster_info_clone,
                    bank_forks_clone,
                    slots_to_repair_for_wen_restart.clone(),
                )
                .is_ok());
            })
            .unwrap();
        let mut rng = rand::thread_rng();
        let mut expected_messages = HashMap::new();
        for keypairs in validator_voting_keypairs.iter().skip(1) {
            let node_pubkey = keypairs.node_keypair.pubkey();
            let node = LegacyContactInfo::new_rand(&mut rng, Some(node_pubkey));
            let last_vote_hash = Hash::new_unique();
            let slots = RestartLastVotedForkSlots::new(
                node_pubkey,
                timestamp(),
                &expected_slots_to_repair,
                last_vote_hash,
                shred_version,
            )
            .unwrap();
            let entries = vec![
                CrdsValue::new_signed(CrdsData::LegacyContactInfo(node), &keypairs.node_keypair),
                CrdsValue::new_signed(
                    CrdsData::RestartLastVotedForkSlots(slots),
                    &keypairs.node_keypair,
                ),
            ];
            {
                let mut gossip_crds = cluster_info.gossip.crds.write().unwrap();
                for entry in entries {
                    assert!(gossip_crds
                        .insert(entry, /*now=*/ 0, GossipRoute::LocalMessage)
                        .is_ok());
                }
            }
            expected_messages.insert(
                node_pubkey.to_string(),
                LastVotedForkSlotsRecord {
                    last_vote_fork_slots: expected_slots_to_repair.clone(),
                    last_vote_bankhash: last_vote_hash.to_string(),
                    shred_version: shred_version as u32,
                },
            );
        }
        let mut parent_bank = bank_forks.read().unwrap().root_bank();
        for slot in expected_slots_to_repair {
            let mut my_bank_forks = bank_forks.write().unwrap();
            my_bank_forks.insert(Bank::new_from_parent(
                parent_bank.clone(),
                &Pubkey::default(),
                slot,
            ));
            parent_bank = my_bank_forks.get(slot).unwrap();
            parent_bank.freeze();
        }
        let _ = wen_restart_thread_handle.join();
        let buffer = read(wen_restart_proto_path).unwrap();
        let progress = WenRestartProgress::decode(&mut std::io::Cursor::new(buffer)).unwrap();
        last_vote_fork_slots.sort();
        last_vote_fork_slots.reverse();
        assert_eq!(
            progress,
            WenRestartProgress {
                state: RestartState::Done.into(),
                my_last_voted_fork_slots: Some(LastVotedForkSlotsRecord {
                    last_vote_fork_slots,
                    last_vote_bankhash: last_vote_bankhash.to_string(),
                    shred_version: 2,
                }),
                last_voted_fork_slots_aggregate: Some(LastVotedForkSlotsAggregateRecord {
                    received: expected_messages
                }),
            }
        )
    }
}
