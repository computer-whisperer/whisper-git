//! Shared helpers for polling optional per-tab receivers.

use std::sync::mpsc::{Receiver, TryRecvError};

/// Result of polling a receiver slot.
pub(crate) enum ReceiverPoll<T> {
    Ready(T),
    Pending,
    Disconnected,
}

/// Poll a receiver slot and clear it on completion/disconnect.
pub(crate) fn poll_slot<T>(slot: &mut Option<Receiver<T>>) -> ReceiverPoll<T> {
    let Some(rx) = slot.as_ref() else {
        return ReceiverPoll::Pending;
    };

    match rx.try_recv() {
        Ok(value) => {
            *slot = None;
            ReceiverPoll::Ready(value)
        }
        Err(TryRecvError::Disconnected) => {
            *slot = None;
            ReceiverPoll::Disconnected
        }
        Err(TryRecvError::Empty) => ReceiverPoll::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::{ReceiverPoll, poll_slot};

    #[test]
    fn in_flight_result_survives_active_tab_switch() {
        let (tx_tab0, rx_tab0) = std::sync::mpsc::channel::<u32>();
        let (_tx_tab1, rx_tab1) = std::sync::mpsc::channel::<u32>();

        let mut slots = [Some(rx_tab0), Some(rx_tab1)];

        // Tab 0 finishes while the user has already switched to tab 1.
        let mut active_tab = 1usize;
        tx_tab0.send(42).unwrap();

        let mut delivered = Vec::new();
        for (idx, slot) in slots.iter_mut().enumerate() {
            if let ReceiverPoll::Ready(value) = poll_slot(slot) {
                delivered.push((idx, value));
            }
        }

        assert_eq!(active_tab, 1);
        assert_eq!(delivered, vec![(0, 42)]);
        assert!(slots[0].is_none());
        assert!(slots[1].is_some());

        // Keep compiler honest that active tab can continue changing independently.
        active_tab = 0;
        assert_eq!(active_tab, 0);
    }

    #[test]
    fn disconnected_slot_is_cleared() {
        let (tx, rx) = std::sync::mpsc::channel::<u32>();
        drop(tx);
        let mut slot = Some(rx);

        assert!(matches!(poll_slot(&mut slot), ReceiverPoll::Disconnected));
        assert!(slot.is_none());
    }
}
