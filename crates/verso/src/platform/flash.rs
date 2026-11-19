//! Deferred completion tracking for flashing e-ink refreshes.
//!
//! Full (GC16) refreshes take ~300 ms on the panel. Rather than blocking the UI
//! thread right after submitting the flash, we record its driver marker and
//! wait for it just before the next framebuffer write, sleep handoff, or exit.
//! Events and position saves keep running during the flash, while nothing can
//! overwrite the framebuffer before the flash finishes.

#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct FlashFence {
    pending: Option<u32>,
}

impl FlashFence {
    /// Records a flashing refresh whose completion has not been waited for yet.
    pub fn mark_flash(&mut self, marker: u32) {
        if marker != 0 {
            self.pending = Some(marker);
        }
    }

    /// Returns the pending flash marker to wait for before writing new pixels
    /// into the framebuffer (or `None` if no unawaited flash is running).
    pub fn take_before_write(&mut self) -> Option<u32> {
        self.pending.take()
    }

    /// Returns the markers to wait for at a full completion barrier (sleep
    /// handoff or exit), given the last refresh marker sent to the driver.
    /// Avoids waiting twice when the pending flash is already the latest update.
    pub fn take_for_barrier(&mut self, last_marker: u32) -> (Option<u32>, Option<u32>) {
        let pending = self.pending.take();
        let tail = (last_marker != 0 && pending != Some(last_marker)).then_some(last_marker);
        (pending, tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_is_awaited_once_before_the_next_write() {
        let mut fence = FlashFence::default();
        assert_eq!(fence.take_before_write(), None);

        fence.mark_flash(42);
        assert_eq!(fence.take_before_write(), Some(42));
        assert_eq!(fence.take_before_write(), None);
    }

    #[test]
    fn zero_marker_is_ignored() {
        let mut fence = FlashFence::default();
        fence.mark_flash(0);
        assert_eq!(fence.take_before_write(), None);
        assert_eq!(fence.take_for_barrier(0), (None, None));
    }

    #[test]
    fn barrier_waits_once_when_pending_flash_is_the_last_refresh() {
        let mut fence = FlashFence::default();
        fence.mark_flash(17);
        assert_eq!(fence.take_for_barrier(17), (Some(17), None));
        assert_eq!(fence.take_before_write(), None);
    }

    #[test]
    fn barrier_waits_for_last_non_flash_refresh_when_no_flash_is_pending() {
        let mut fence = FlashFence::default();
        fence.mark_flash(17);
        assert_eq!(fence.take_before_write(), Some(17));
        // A non-flashing refresh (marker 18) followed the awaited flash.
        assert_eq!(fence.take_for_barrier(18), (None, Some(18)));
    }

    #[test]
    fn barrier_waits_for_both_if_another_marker_followed_an_unawaited_flash() {
        let mut fence = FlashFence::default();
        fence.mark_flash(17);
        assert_eq!(fence.take_for_barrier(18), (Some(17), Some(18)));
        assert_eq!(fence.take_before_write(), None);
    }
}
