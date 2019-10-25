use crate::cuda_runtime::PinnedVec;
use crate::packet::{Packet, Packets};
use crate::recycler::Recycler;
use crate::recycler::Reset;
use crate::sigverify::{batch_size, TxOffset};
use bincode::deserialize;
use rayon::iter::IntoParallelIterator;
use rayon::iter::ParallelIterator;
use rayon::ThreadPool;
use solana_ledger::perf_libs;
use solana_ledger::shred::ShredType;
use solana_metrics::inc_new_counter_debug;
use solana_rayon_threadlimit::get_thread_count;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use std::collections::HashMap;
use std::mem::size_of;

use std::cell::RefCell;

thread_local!(static PAR_THREAD_POOL: RefCell<ThreadPool> = RefCell::new(rayon::ThreadPoolBuilder::new()
                    .num_threads(get_thread_count())
                    .thread_name(|ix| format!("sigverify_shreds_{}", ix))
                    .build()
                    .unwrap()));

impl Reset for PinnedVec<Pubkey> {
    fn reset(&mut self) {
        self.resize(0, Pubkey::default());
    }
}

/// Assuming layout is
/// signature: Signature
/// signed_msg: {
///   type: ShredType
///   slot: u64,
///   ...
/// }
/// Signature is the first thing in the packet, and slot is the first thing in the signed message.
fn verify_shred_cpu(packet: &Packet, slot_leaders: &HashMap<u64, Pubkey>) -> Option<u8> {
    let sig_start = 0;
    let sig_end = size_of::<Signature>();
    let slot_start = sig_end + size_of::<ShredType>();
    let slot_end = slot_start + size_of::<u64>();
    let msg_start = sig_end;
    let msg_end = packet.meta.size;
    trace!("slot start and end {} {}", slot_start, slot_end);
    let slot: u64 = deserialize(&packet.data[slot_start..slot_end]).ok()?;
    trace!("slot {}", slot);
    let pubkey: &Pubkey = slot_leaders.get(&slot)?;
    let signature = Signature::new(&packet.data[sig_start..sig_end]);
    trace!("signature {}", signature);
    if !signature.verify(pubkey.as_ref(), &packet.data[msg_start..msg_end]) {
        return Some(0);
    }
    return Some(1);
}

pub fn verify_shreds_cpu(batches: &[Packets], slot_leaders: &HashMap<u64, Pubkey>) -> Vec<Vec<u8>> {
    use rayon::prelude::*;
    let count = batch_size(batches);
    debug!("CPU SHRED ECDSA for {}", count);
    let rv = PAR_THREAD_POOL.with(|thread_pool| {
        thread_pool.borrow().install(|| {
            batches
                .into_par_iter()
                .map(|p| {
                    p.packets
                        .par_iter()
                        .map(|p| verify_shred_cpu(p, slot_leaders).unwrap_or(0))
                        .collect()
                })
                .collect()
        })
    });
    inc_new_counter_debug!("ed25519_shred_verify_cpu", count);
    rv
}

fn shred_gpu_pubkeys(
    batches: &[Packets],
    slot_leaders: &HashMap<u64, Pubkey>,
    recycler_offsets: &Recycler<TxOffset>,
    recycler_pubkeys: &Recycler<PinnedVec<Pubkey>>,
) -> (PinnedVec<Pubkey>, TxOffset) {
    //TODO: mark Pubkey::default shreds as failed after the GPU returns
    assert_eq!(slot_leaders.get(&std::u64::MAX), Some(&Pubkey::default()));
    let slots: Vec<Vec<u64>> = PAR_THREAD_POOL.with(|thread_pool| {
        thread_pool.borrow().install(|| {
            batches
                .into_par_iter()
                .map(|p| {
                    p.packets
                        .iter()
                        .map(|packet| {
                            let slot_start = size_of::<Signature>() + size_of::<ShredType>();
                            let slot_end = slot_start + size_of::<u64>();
                            let slot: Option<u64> =
                                deserialize(&packet.data[slot_start..slot_end]).ok();
                            if slot.is_some() && slot_leaders.get(&slot.unwrap()).is_some() {
                                slot.unwrap()
                            } else {
                                std::u64::MAX
                            }
                        })
                        .collect()
                })
                .collect()
        })
    });
    let mut keys: HashMap<Pubkey, Vec<u64>> = HashMap::new();
    for batch in slots.iter() {
        for slot in batch.iter() {
            let key = slot_leaders.get(slot).unwrap();
            keys.entry(*key).or_insert(vec![]).push(*slot);
        }
    }
    let mut pubkeys: PinnedVec<Pubkey> = recycler_pubkeys.allocate("shred_gpu_pubkeys");
    let mut slot_to_key_ix = HashMap::new();
    for (i, (k, slots)) in keys.iter().enumerate() {
        pubkeys.push(*k);
        for s in slots {
            slot_to_key_ix.insert(s, i);
        }
    }
    let mut offsets = recycler_offsets.allocate("shred_offsets");
    slots.iter().for_each(|packet_slots| {
        packet_slots.iter().for_each(|slot| {
            offsets.push((slot_to_key_ix.get(slot).unwrap() * size_of::<Pubkey>()) as u32);
        });
    });
    //HACK: Pubkeys vector is passed along as a `Packets` buffer to the GPU
    //TODO: GPU needs a more opaque interface, which can handle variable sized structures for data
    //Pad the Pubkeys buffer such that it is bigger than a buffer of Packet sized elems
    let num_in_packets =
        (pubkeys.len() * size_of::<Pubkey>() + (size_of::<Packet>() - 1)) / size_of::<Packet>();
    trace!("num_in_packets {}", num_in_packets);
    //number of bytes missing
    let missing = num_in_packets * size_of::<Packet>() - pubkeys.len() * size_of::<Pubkey>();
    trace!("missing {}", missing);
    //extra Pubkeys needed to fill the buffer
    let extra = (missing + size_of::<Pubkey>() - 1) / size_of::<Pubkey>();
    trace!("extra {}", extra);
    trace!("pubkeys {}", pubkeys.len());
    for _ in 0..extra {
        pubkeys.push(Pubkey::default());
        trace!("pubkeys {}", pubkeys.len());
    }
    trace!("pubkeys {:?}", pubkeys);
    trace!("offsets {:?}", offsets);
    (pubkeys, offsets)
}

fn shred_gpu_offsets(
    mut pubkeys_end: u32,
    batches: &[Packets],
    recycler_offsets: &Recycler<TxOffset>,
) -> (TxOffset, TxOffset, TxOffset, Vec<Vec<u32>>) {
    let mut signature_offsets = recycler_offsets.allocate("shred_signatures");
    let mut msg_start_offsets = recycler_offsets.allocate("shred_msg_starts");
    let mut msg_sizes = recycler_offsets.allocate("shred_msg_sizes");
    let mut v_sig_lens = vec![];
    for batch in batches {
        let mut sig_lens = Vec::new();
        for packet in &batch.packets {
            let sig_start = pubkeys_end;
            let sig_end = sig_start + size_of::<Signature>() as u32;
            let msg_start = sig_end;
            let msg_end = sig_start + packet.meta.size as u32;
            signature_offsets.push(sig_start);
            msg_start_offsets.push(msg_start);
            msg_sizes.push(msg_end - msg_start);
            sig_lens.push(1);
            pubkeys_end += size_of::<Packet>() as u32;
        }
        v_sig_lens.push(sig_lens);
    }
    (signature_offsets, msg_start_offsets, msg_sizes, v_sig_lens)
}

pub fn verify_shreds_gpu(
    batches: &[Packets],
    slot_leaders: &HashMap<u64, Pubkey>,
    recycler_offsets: &Recycler<TxOffset>,
    recycler_pubkeys: &Recycler<PinnedVec<Pubkey>>,
    recycler_out: &Recycler<PinnedVec<u8>>,
) -> Vec<Vec<u8>> {
    let api = perf_libs::api();
    assert!(api.is_some());
    if api.is_none() {
        return verify_shreds_cpu(batches, slot_leaders);
    }
    let api = api.unwrap();

    let mut elems = Vec::new();
    let mut rvs = Vec::new();
    let count = batch_size(batches);
    let (pubkeys, pubkey_offsets) =
        shred_gpu_pubkeys(batches, slot_leaders, recycler_offsets, recycler_pubkeys);
    //HACK: Pubkeys vector is passed along as a `Packets` buffer to the GPU
    //TODO: GPU needs a more opaque interface, which can handle variable sized structures for data
    let mut num_packets =
        (pubkeys.len() * size_of::<Pubkey>() + size_of::<Packet>() - 1) / size_of::<Packet>();
    let pubkeys_len = (num_packets * size_of::<Packet>()) as u32;
    trace!("num_packets: {}", num_packets);
    trace!("pubkeys_len: {}", pubkeys_len);
    let (signature_offsets, msg_start_offsets, msg_sizes, v_sig_lens) =
        shred_gpu_offsets(pubkeys_len, batches, recycler_offsets);
    let mut out = recycler_out.allocate("out_buffer");
    out.set_pinnable();
    elems.push(perf_libs::Elems {
        elems: pubkeys.as_ptr() as *const solana_sdk::packet::Packet,
        num: num_packets as u32,
    });

    for p in batches {
        elems.push(perf_libs::Elems {
            elems: p.packets.as_ptr(),
            num: p.packets.len() as u32,
        });
        let mut v = Vec::new();
        v.resize(p.packets.len(), 0);
        rvs.push(v);
        num_packets += p.packets.len();
    }
    out.resize(signature_offsets.len(), 0);

    use crate::packet::PACKET_DATA_SIZE;
    trace!("Starting verify num packets: {}", num_packets);
    trace!("elem len: {}", elems.len() as u32);
    trace!("packet sizeof: {}", size_of::<Packet>() as u32);
    trace!("len offset: {}", PACKET_DATA_SIZE as u32);
    const USE_NON_DEFAULT_STREAM: u8 = 1;
    unsafe {
        let res = (api.ed25519_verify_many)(
            elems.as_ptr(),
            elems.len() as u32,
            size_of::<Packet>() as u32,
            num_packets as u32,
            signature_offsets.len() as u32,
            msg_sizes.as_ptr(),
            pubkey_offsets.as_ptr(),
            signature_offsets.as_ptr(),
            msg_start_offsets.as_ptr(),
            out.as_mut_ptr(),
            USE_NON_DEFAULT_STREAM,
        );
        if res != 0 {
            trace!("RETURN!!!: {}", res);
        }
    }
    trace!("done verify");
    trace!("out buf {:?}", out);

    let mut num = 0;
    for (vs, sig_vs) in rvs.iter_mut().zip(v_sig_lens.iter()) {
        for (v, sig_v) in vs.iter_mut().zip(sig_vs.iter()) {
            if *sig_v == 0 {
                *v = 0;
            } else {
                let mut vout = 1;
                for _ in 0..*sig_v {
                    if 0 == out[num] {
                        vout = 0;
                    }
                    num += 1;
                }
                *v = vout;
            }
            if *v != 0 {
                trace!("VERIFIED SHRED!!!!!");
            }
        }
    }
    inc_new_counter_debug!("ed25519_shred_verify_gpu", count);
    recycler_out.recycle(out);
    recycler_offsets.recycle(signature_offsets);
    recycler_offsets.recycle(pubkey_offsets);
    recycler_offsets.recycle(msg_sizes);
    recycler_offsets.recycle(msg_start_offsets);
    recycler_pubkeys.recycle(pubkeys);
    rvs
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use solana_ledger::shred::{Shred, Shredder};
    use solana_sdk::signature::{Keypair, KeypairUtil};
    #[test]
    fn test_sigverify_shred_cpu() {
        solana_logger::setup();
        let mut packet = Packet::default();
        let slot = 0xdeadc0de;
        let mut shred = Shred::new_from_data(slot, 0xc0de, 0xdead, Some(&[1, 2, 3, 4]), true, true);
        assert_eq!(shred.slot(), slot);
        let keypair = Keypair::new();
        Shredder::sign_shred(&keypair, &mut shred);
        trace!("signature {}", shred.common_header.signature);
        packet.data[0..shred.payload.len()].copy_from_slice(&shred.payload);
        packet.meta.size = shred.payload.len();

        let mut leader_slots: HashMap<u64, Pubkey> = HashMap::new();
        leader_slots.insert(slot, keypair.pubkey());
        let rv = verify_shred_cpu(&packet, &leader_slots);
        assert_eq!(rv, Some(1));

        let wrong_keypair = Keypair::new();
        leader_slots.insert(slot, wrong_keypair.pubkey());
        let rv = verify_shred_cpu(&packet, &leader_slots);
        assert_eq!(rv, Some(0));

        leader_slots.remove(&slot);
        let rv = verify_shred_cpu(&packet, &leader_slots);
        assert_eq!(rv, None);
    }

    #[test]
    fn test_sigverify_shreds_cpu() {
        solana_logger::setup();
        let mut batch = [Packets::default()];
        let slot = 0xdeadc0de;
        let mut shred = Shred::new_from_data(slot, 0xc0de, 0xdead, Some(&[1, 2, 3, 4]), true, true);
        let keypair = Keypair::new();
        Shredder::sign_shred(&keypair, &mut shred);
        batch[0].packets.resize(1, Packet::default());
        batch[0].packets[0].data[0..shred.payload.len()].copy_from_slice(&shred.payload);
        batch[0].packets[0].meta.size = shred.payload.len();

        let mut leader_slots: HashMap<u64, Pubkey> = HashMap::new();
        leader_slots.insert(slot, keypair.pubkey());
        let rv = verify_shreds_cpu(&batch, &leader_slots);
        assert_eq!(rv, vec![vec![1]]);

        let wrong_keypair = Keypair::new();
        leader_slots.insert(slot, wrong_keypair.pubkey());
        let rv = verify_shreds_cpu(&batch, &leader_slots);
        assert_eq!(rv, vec![vec![0]]);

        leader_slots.remove(&slot);
        let rv = verify_shreds_cpu(&batch, &leader_slots);
        assert_eq!(rv, vec![vec![0]]);
    }

    #[test]
    fn test_sigverify_shreds_gpu() {
        solana_logger::setup();
        let recycler_offsets = Recycler::default();
        let recycler_pubkeys = Recycler::default();
        let recycler_out = Recycler::default();
        let mut leader_slots: HashMap<u64, Pubkey> = HashMap::new();
        leader_slots.insert(std::u64::MAX, Pubkey::default());

        let mut batch = [Packets::default()];
        let slot = 0xdeadc0de;
        let mut shred = Shred::new_from_data(slot, 0xc0de, 0xdead, Some(&[1, 2, 3, 4]), true, true);
        let keypair = Keypair::new();
        Shredder::sign_shred(&keypair, &mut shred);
        batch[0].packets.resize(1, Packet::default());
        batch[0].packets[0].data[0..shred.payload.len()].copy_from_slice(&shred.payload);
        batch[0].packets[0].meta.size = shred.payload.len();

        leader_slots.insert(slot, keypair.pubkey());
        let rv = verify_shreds_gpu(
            &batch,
            &leader_slots,
            &recycler_offsets,
            &recycler_pubkeys,
            &recycler_out,
        );
        assert_eq!(rv, vec![vec![1]]);

        let wrong_keypair = Keypair::new();
        leader_slots.insert(slot, wrong_keypair.pubkey());
        let rv = verify_shreds_gpu(
            &batch,
            &leader_slots,
            &recycler_offsets,
            &recycler_pubkeys,
            &recycler_out,
        );
        assert_eq!(rv, vec![vec![0]]);

        leader_slots.remove(&slot);
        let rv = verify_shreds_gpu(
            &batch,
            &leader_slots,
            &recycler_offsets,
            &recycler_pubkeys,
            &recycler_out,
        );
        assert_eq!(rv, vec![vec![0]]);
    }
}
