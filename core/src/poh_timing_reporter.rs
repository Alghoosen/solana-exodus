use {
    solana_metrics::{datapoint::PohTimingPoint, datapoint_info},
    solana_sdk::clock::Slot,
    std::{collections::HashMap, fmt},
};

/// A SlotPohTimestamp records timing of the events during the processing of a
/// slot by the validator
#[derive(Debug, Clone, Copy, Default)]
pub struct SlotPohTimestamp {
    /// Slot start time from poh
    pub start_time: u64,
    /// Slot end time from poh
    pub end_time: u64,
    /// Last shred received time from block producer
    pub full_time: u64,
}

/// Display trait
impl fmt::Display for SlotPohTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SlotPohTimestamp: start={} end={} full={}",
            self.start_time, self.end_time, self.full_time
        )
    }
}

impl SlotPohTimestamp {
    /// Return true if the timing points of all events are received.
    pub fn is_complete(&self) -> bool {
        self.start_time != 0 && self.end_time != 0 && self.full_time != 0
    }

    /// Update with timing point
    pub fn update(&mut self, timing_point: PohTimingPoint) {
        match timing_point {
            PohTimingPoint::PohSlotStart(ts) => self.start_time = ts,
            PohTimingPoint::PohSlotEnd(ts) => self.end_time = ts,
            PohTimingPoint::FullSlotReceived(ts) => self.full_time = ts,
        }
    }

    /// Return the time difference from slot full to slot start
    pub fn slot_full_to_start_time(&self) -> i64 {
        (self.start_time as i64).saturating_sub(self.full_time as i64)
    }

    /// Return the time difference from slot full to slot end
    pub fn slot_full_to_end_time(&self) -> i64 {
        (self.end_time as i64).saturating_sub(self.full_time as i64)
    }
}

/// A PohTimingReporter manages and reports the timing of events for incoming
/// slots
#[derive(Default)]
pub struct PohTimingReporter {
    /// Storage map of SlotPohTimestamp per slot
    slot_timestamps: HashMap<Slot, SlotPohTimestamp>,
    last_root_slot: Slot,
}

impl PohTimingReporter {
    /// Return true if PohTiming is complete for the slot
    pub fn is_complete(&self, slot: Slot) -> bool {
        if let Some(slot_timestamp) = self.slot_timestamps.get(&slot) {
            slot_timestamp.is_complete()
        } else {
            false
        }
    }

    /// Report PohTiming for a slot
    pub fn report(&self, slot: Slot, slot_timestamp: &SlotPohTimestamp) {
        datapoint_info!(
            "poh_slot_timing",
            ("slot", slot as i64, i64),
            ("start_time", slot_timestamp.start_time as i64, i64),
            ("end_time", slot_timestamp.end_time as i64, i64),
            ("full_time", slot_timestamp.full_time as i64, i64),
            (
                "full_to_start_time_diff",
                slot_timestamp.slot_full_to_start_time(),
                i64
            ),
            (
                "full_to_end_time_diff",
                slot_timestamp.slot_full_to_end_time(),
                i64
            ),
        );
    }

    /// Process incoming PohTimingPoint from the channel
    pub fn process(&mut self, slot: Slot, root_slot: Option<Slot>, t: PohTimingPoint) -> bool {
        let mut completed = false;
        let slot_timestamp = self
            .slot_timestamps
            .entry(slot)
            .or_insert_with(SlotPohTimestamp::default);

        slot_timestamp.update(t);
        if let Some(slot_timestamp) = self.slot_timestamps.get(&slot) {
            if slot_timestamp.is_complete() {
                self.report(slot, slot_timestamp);
                completed = true;
            }
        }

        // delete slots that are older than the root_slot
        if let Some(root_slot) = root_slot {
            if root_slot != self.last_root_slot {
                self.slot_timestamps.retain(|&k, _| k >= root_slot);
                self.last_root_slot = root_slot;
            }
        }
        completed
    }

    /// Return the size of slot_timestamps in memory
    pub fn timestamp_size(&self) -> usize {
        self.slot_timestamps.len()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    /// Test poh_timing_reporter
    fn test_poh_timing_reporter() {
        // create a reporter
        let mut reporter = PohTimingReporter::default();

        // process all relevant PohTimingPoints for slot 42
        let complete = reporter.process(42, None, PohTimingPoint::PohSlotStart(100));
        assert!(!complete);
        let complete = reporter.process(42, None, PohTimingPoint::PohSlotEnd(200));
        assert!(!complete);
        let complete = reporter.process(42, None, PohTimingPoint::FullSlotReceived(150));
        // assert that the PohTiming is complete
        assert!(complete);

        // Move root to slot 43
        let root = Some(43);

        // process all relevant PohTimingPoints for slot 45
        let complete = reporter.process(45, None, PohTimingPoint::PohSlotStart(100));
        assert!(!complete);
        let complete = reporter.process(45, None, PohTimingPoint::PohSlotEnd(200));
        assert!(!complete);
        let complete = reporter.process(45, root, PohTimingPoint::FullSlotReceived(150));
        // assert that the PohTiming is complete
        assert!(complete);

        // assert that only one timestamp is kept
        assert_eq!(reporter.timestamp_size(), 1)
    }

    #[test]
    fn test_poh_timing_reporter_overflow() {
        // create a reporter
        let mut reporter = PohTimingReporter::default();

        // process all relevant PohTimingPoints for a slot
        let complete = reporter.process(42, None, PohTimingPoint::PohSlotStart(1647624609896));
        assert!(!complete);
        let complete = reporter.process(42, None, PohTimingPoint::PohSlotEnd(1647624610286));
        assert!(!complete);
        let complete = reporter.process(42, None, PohTimingPoint::FullSlotReceived(1647624610281));

        // assert that the PohTiming is complete
        assert!(complete);
    }

    #[test]
    fn test_slot_poh_timestamp_fmt() {
        let t = SlotPohTimestamp::default();
        assert_eq!(format!("{}", t), "SlotPohTimestamp: start=0 end=0 full=0");
    }
}
