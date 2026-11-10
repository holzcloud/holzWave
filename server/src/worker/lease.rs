//! The writer lease of a session: who may type, who only watches, and when the right to type is free.
//!
//! One attachment at a time holds the lease of a session. Input and resizes of every other attachment are dropped, so a
//! watcher can never move the program. The holder's silence of [`WRITER_TIMEOUT_MS`] frees the lease; it never moves to a
//! watcher by itself, so the person who is typing is never surprised by a second pair of hands. An explicit
//! [`Lease::takeover`] by any attachment of the session moves it, and every attachment is told its own view of the change.
//!
//! This is plain data and functions: no thread, no clock. Every call that depends on time takes the time in milliseconds as
//! an argument, so a test advances time by passing numbers and the worker passes a monotonic clock. A clock that goes
//! backwards frees nothing and breaks nothing.
//!
//! Every call that can change a view returns the changes as a list of `(attachment, new view)`, for the attachments whose
//! view really changed and for no other, so the caller sends exactly one event per change.

use std::collections::BTreeMap;

/// How long the holder of a lease may give no sign of life before the lease is free, in milliseconds. Input, a resize, a
/// takeover and a beat are signs of life.
pub const WRITER_TIMEOUT_MS: u64 = 30_000;

/// How often a client that is attached says it is alive, in milliseconds. The client's number, not the lease's: it is
/// written here so that the two ends of the rule stand side by side. A beat every third of the timeout lets two beats be
/// lost before the lease is freed.
pub const BEAT_INTERVAL_MS: u64 = 10_000;

/// An attachment: one client's view of one session. Opaque to the lease; the owner of the sessions hands them out and
/// never reuses one while it lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Who(pub u64);

/// What an attachment is told about the right to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriterState {
    /// This attachment holds the lease: its input reaches the program.
    Writing,
    /// Another attachment holds it: this one sees the session and its input is dropped.
    Watching,
    /// Nobody holds it: input is dropped until an attachment takes over.
    Free,
}

impl WriterState {
    /// The word that names the state on every wire and in every label.
    pub fn word(self) -> &'static str {
        match self {
            WriterState::Writing => "writing",
            WriterState::Watching => "watching",
            WriterState::Free => "free",
        }
    }
}

/// The views that changed: the attachment and what it is told now.
pub type Changes = Vec<(Who, WriterState)>;

/// What became of an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// The sender holds the lease: the bytes go to the program, and the input was a sign of life.
    Accepted,
    /// The sender does not hold the lease (or is not attached): nothing reaches the program.
    Dropped,
}

/// A takeover that cannot be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The attachment is not attached to this session.
    NotAttached,
}

/// The lease of one session.
#[derive(Clone, Debug)]
pub struct Lease {
    /// Every attachment, with the view it was told last.
    told: BTreeMap<Who, WriterState>,
    holder: Option<Who>,
    /// The time of the holder's last sign of life; meaningful while there is a holder.
    last_life: u64,
    timeout_ms: u64,
}

impl Default for Lease {
    fn default() -> Lease {
        Lease::new(WRITER_TIMEOUT_MS)
    }
}

impl Lease {
    /// An empty lease that frees after `timeout_ms` of silence of its holder.
    pub fn new(timeout_ms: u64) -> Lease {
        Lease { told: BTreeMap::new(), holder: None, last_life: 0, timeout_ms }
    }

    /// The holder, if there is one.
    pub fn holder(&self) -> Option<Who> {
        self.holder
    }

    /// How many attachments there are.
    pub fn attached(&self) -> usize {
        self.told.len()
    }

    /// Whether `who` is attached.
    pub fn is_attached(&self, who: Who) -> bool {
        self.told.contains_key(&who)
    }

    /// The attachments, in order.
    pub fn members(&self) -> impl Iterator<Item = Who> + '_ {
        self.told.keys().copied()
    }

    /// What `who` is told: `None` for an attachment that is not here.
    pub fn view(&self, who: Who) -> Option<WriterState> {
        self.told.contains_key(&who).then(|| self.view_of(who))
    }

    fn view_of(&self, who: Who) -> WriterState {
        match self.holder {
            Some(holder) if holder == who => WriterState::Writing,
            Some(_) => WriterState::Watching,
            None => WriterState::Free,
        }
    }

    /// The views that differ from what each attachment was told last, which are then taken as told.
    fn settle(&mut self) -> Changes {
        let holder = self.holder;
        let mut changes = Changes::new();
        for (who, told) in &mut self.told {
            let now = match holder {
                Some(h) if h == *who => WriterState::Writing,
                Some(_) => WriterState::Watching,
                None => WriterState::Free,
            };
            if *told != now {
                *told = now;
                changes.push((*who, now));
            }
        }
        changes
    }

    /// Time never runs backwards for the lease: a smaller value than the last one counts as the last one.
    fn life(&mut self, now: u64) {
        self.last_life = self.last_life.max(now);
    }

    /// Attaches `who`. With `claim` (only the creator of a session that has no holder yet) it takes the lease; without it,
    /// or when someone else holds it already, it watches, and a session with no holder shows `Free`. The first entry of the
    /// answer is always the new attachment's own view; the rest are the views that this changed for the others.
    pub fn attach(&mut self, who: Who, claim: bool, now: u64) -> Changes {
        if self.told.contains_key(&who) {
            return Changes::new();
        }
        if claim && self.holder.is_none() {
            self.holder = Some(who);
            self.last_life = now;
        }
        // The others first, while the new one is not among them: a session that nobody held, and that the creator claims
        // now, turns the attachments that showed `Free` into watchers.
        let own = self.view_of(who);
        let mut changes = self.settle();
        self.told.insert(who, own);
        changes.insert(0, (who, own));
        changes
    }

    /// `who` leaves. When it held the lease the lease is free and the others are told so; a watcher that leaves changes
    /// nothing for anyone.
    pub fn detach(&mut self, who: Who) -> Changes {
        if self.told.remove(&who).is_none() {
            return Changes::new();
        }
        if self.holder == Some(who) {
            self.holder = None;
        }
        self.settle()
    }

    /// The socket of `who` ended: the same as [`Lease::detach`].
    pub fn gone(&mut self, who: Who) -> Changes {
        self.detach(who)
    }

    /// A sign of life. Only the holder's counts: a watcher's beat does not extend the holder's time.
    pub fn beat(&mut self, who: Who, now: u64) {
        if self.holder == Some(who) {
            self.life(now);
        }
    }

    /// An input of `who`. The holder's is accepted and is a sign of life; every other one is dropped.
    pub fn input(&mut self, who: Who, now: u64) -> Input {
        if self.holder == Some(who) && self.told.contains_key(&who) {
            self.life(now);
            Input::Accepted
        } else {
            Input::Dropped
        }
    }

    /// Whether `who` may change the size of the session: only the holder. A resize is a sign of life, which the caller
    /// reports with [`Lease::beat`] once it has done the resize.
    pub fn resize_allowed(&self, who: Who) -> bool {
        self.holder == Some(who) && self.told.contains_key(&who)
    }

    /// `who` takes the lease, whoever held it. Any attachment of the session may. Two takeovers in a row leave the later one
    /// holding. A takeover by an attachment that is not here is refused.
    pub fn takeover(&mut self, who: Who, now: u64) -> Result<Changes, Refused> {
        if !self.told.contains_key(&who) {
            return Err(Refused::NotAttached);
        }
        self.holder = Some(who);
        self.last_life = now;
        Ok(self.settle())
    }

    /// The time at which a holder that stays silent loses the lease, or `None` while nobody holds it. The holder's next
    /// sign of life moves it later, so it is the earliest time a [`Lease::sweep`] can free anything.
    pub fn deadline(&self) -> Option<u64> {
        self.holder.map(|_| self.last_life.saturating_add(self.timeout_ms))
    }

    /// The clock has reached `now`: a holder that was silent for the whole timeout loses the lease and nobody gets it.
    pub fn sweep(&mut self, now: u64) -> Changes {
        if self.holder.is_some() && now.saturating_sub(self.last_life) >= self.timeout_ms {
            self.holder = None;
            return self.settle();
        }
        Changes::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use WriterState::{Free, Watching, Writing};

    const A: Who = Who(1);
    const B: Who = Who(2);
    const C: Who = Who(3);

    /// A lease created and claimed by `A`, with `B` and `C` watching, all at time 0.
    fn three() -> Lease {
        let mut lease = Lease::default();
        assert_eq!(lease.attach(A, true, 0), vec![(A, Writing)]);
        assert_eq!(lease.attach(B, false, 0), vec![(B, Watching)]);
        assert_eq!(lease.attach(C, false, 0), vec![(C, Watching)]);
        lease
    }

    #[test]
    fn the_creator_writes_and_every_later_attachment_watches() {
        let lease = three();
        assert_eq!(lease.view(A), Some(Writing));
        assert_eq!(lease.view(B), Some(Watching));
        assert_eq!(lease.view(C), Some(Watching));
        assert_eq!(lease.holder(), Some(A));
        assert_eq!(lease.view(Who(9)), None, "an attachment that is not here has no view");
    }

    #[test]
    fn an_attachment_to_a_session_with_no_holder_is_free() {
        let mut lease = Lease::default();
        assert_eq!(lease.attach(A, false, 5), vec![(A, Free)], "nobody claimed it");
        assert_eq!(lease.attach(B, false, 6), vec![(B, Free)]);
        assert_eq!(lease.holder(), None);
        // A claim on a session that has a holder is no claim.
        let mut taken = three();
        assert_eq!(taken.attach(Who(4), true, 1), vec![(Who(4), Watching)]);
        assert_eq!(taken.holder(), Some(A));
    }

    #[test]
    fn a_second_attach_of_the_same_attachment_changes_nothing() {
        let mut lease = three();
        assert!(lease.attach(B, true, 10).is_empty());
        assert_eq!(lease.attached(), 3);
        assert_eq!(lease.holder(), Some(A));
    }

    #[test]
    fn input_and_resize_of_a_watcher_are_dropped_and_the_holders_are_not() {
        let mut lease = three();
        assert_eq!(lease.input(B, 10), Input::Dropped);
        assert_eq!(lease.input(C, 10), Input::Dropped);
        assert_eq!(lease.input(Who(9), 10), Input::Dropped, "an unknown attachment");
        assert_eq!(lease.input(A, 10), Input::Accepted);
        assert!(lease.resize_allowed(A));
        assert!(!lease.resize_allowed(B));
        assert!(!lease.resize_allowed(Who(9)));
    }

    #[test]
    fn input_of_the_holder_is_a_sign_of_life() {
        let mut lease = three();
        assert_eq!(lease.input(A, 20_000), Input::Accepted);
        // 29 999 ms after the input, not after the start.
        assert!(lease.sweep(49_999).is_empty());
        assert_eq!(lease.holder(), Some(A));
        assert!(!lease.sweep(50_000).is_empty());
    }

    #[test]
    fn the_silence_of_the_holder_frees_the_lease_at_the_timeout_and_not_before() {
        let mut lease = three();
        assert!(lease.sweep(29_999).is_empty(), "29 999 ms: still held");
        assert_eq!(lease.holder(), Some(A));
        let changes = lease.sweep(30_000);
        assert_eq!(changes, vec![(A, Free), (B, Free), (C, Free)], "every attachment is told its own view");
        assert_eq!(lease.holder(), None);
        for who in [A, B, C] {
            assert_eq!(lease.view(who), Some(Free));
        }
        assert!(lease.sweep(31_000).is_empty(), "a free lease has nothing to free");
    }

    #[test]
    fn a_free_lease_never_moves_to_a_watcher_by_itself() {
        let mut lease = three();
        lease.sweep(30_000);
        // Whatever the watchers do, nothing makes one of them the holder.
        for t in [30_001, 40_000, 90_000, 1_000_000] {
            assert_eq!(lease.input(B, t), Input::Dropped);
            assert_eq!(lease.input(C, t), Input::Dropped);
            lease.beat(B, t);
            lease.beat(C, t);
            assert!(lease.sweep(t).is_empty());
            assert_eq!(lease.holder(), None);
        }
        // The old holder has no claim either until it takes over.
        assert_eq!(lease.input(A, 1_000_001), Input::Dropped);
    }

    #[test]
    fn a_beat_of_the_holder_extends_its_time_and_a_beat_of_a_watcher_does_not() {
        let mut lease = three();
        lease.beat(B, 25_000);
        lease.beat(C, 29_000);
        assert_eq!(lease.sweep(30_000).len(), 3, "the watchers' beats did not help the holder");

        let mut lease = three();
        lease.beat(A, 25_000);
        assert!(lease.sweep(54_999).is_empty());
        assert_eq!(lease.sweep(55_000).len(), 3);
    }

    #[test]
    fn a_takeover_by_any_attachment_moves_the_lease_and_tells_everyone() {
        let mut lease = three();
        let changes = lease.takeover(B, 1_000).expect("an attached watcher may take over");
        assert_eq!(changes, vec![(A, Watching), (B, Writing)]);
        assert_eq!(lease.holder(), Some(B));
        assert_eq!(lease.input(A, 1_001), Input::Dropped);
        assert_eq!(lease.input(B, 1_001), Input::Accepted);
        // Two in a row: the later one holds.
        lease.takeover(C, 1_002).expect("takeover");
        let changes = lease.takeover(A, 1_003).expect("takeover");
        assert_eq!(changes, vec![(A, Writing), (C, Watching)]);
        assert_eq!(lease.holder(), Some(A));
    }

    #[test]
    fn a_takeover_after_the_lease_became_free_gives_it_to_the_taker() {
        let mut lease = three();
        lease.sweep(30_000);
        let changes = lease.takeover(C, 31_000).expect("takeover");
        assert_eq!(changes, vec![(A, Watching), (B, Watching), (C, Writing)]);
    }

    #[test]
    fn a_takeover_by_the_holder_or_by_an_unknown_attachment() {
        let mut lease = three();
        assert_eq!(lease.takeover(A, 100).expect("the holder"), vec![], "nothing changed");
        assert_eq!(lease.takeover(Who(9), 100), Err(Refused::NotAttached));
        assert_eq!(lease.holder(), Some(A), "a refused takeover moved nothing");
    }

    #[test]
    fn a_holder_that_detaches_or_is_gone_frees_the_lease() {
        let mut lease = three();
        let changes = lease.detach(A);
        assert_eq!(changes, vec![(B, Free), (C, Free)], "the leaver is not told");
        assert_eq!(lease.holder(), None);
        assert_eq!(lease.attached(), 2);

        let mut lease = three();
        assert_eq!(lease.gone(A), vec![(B, Free), (C, Free)]);
    }

    #[test]
    fn a_watcher_that_detaches_changes_nothing_for_the_others() {
        let mut lease = three();
        assert!(lease.detach(B).is_empty());
        assert!(lease.gone(C).is_empty());
        assert_eq!(lease.holder(), Some(A));
        assert!(lease.detach(B).is_empty(), "a second detach is nothing");
        assert!(lease.detach(Who(9)).is_empty());
    }

    #[test]
    fn a_change_lists_only_the_views_that_changed() {
        let mut lease = three();
        // A takeover by the holder, a beat, an accepted input, a sweep before the time: no change, no event.
        assert!(lease.takeover(A, 5).expect("takeover").is_empty());
        lease.beat(A, 6);
        lease.input(A, 7);
        assert!(lease.sweep(8).is_empty());
        // B takes over: C was Watching and is Watching still, so C is not in the list.
        let changes = lease.takeover(B, 9).expect("takeover");
        assert_eq!(changes, vec![(A, Watching), (B, Writing)]);
        assert!(!changes.iter().any(|(who, _)| *who == C));
        // An attach that does not change the others lists only the new one.
        assert_eq!(lease.attach(Who(4), false, 10), vec![(Who(4), Watching)]);
    }

    #[test]
    fn a_clock_that_goes_backwards_frees_nothing_and_does_not_panic() {
        let mut lease = three();
        lease.beat(A, 100_000);
        // Earlier than the last sign of life.
        assert!(lease.sweep(10).is_empty());
        assert_eq!(lease.input(A, 5), Input::Accepted);
        assert!(lease.sweep(0).is_empty());
        assert_eq!(lease.holder(), Some(A));
        // The earlier time did not move the last sign of life back: the old one still counts.
        assert!(lease.sweep(129_999).is_empty());
        assert_eq!(lease.sweep(130_000).len(), 3);
        // The extremes do not overflow.
        let mut lease = three();
        lease.beat(A, u64::MAX);
        assert!(lease.sweep(0).is_empty());
        assert!(lease.sweep(u64::MAX).is_empty());
        let mut lease = Lease::new(u64::MAX);
        lease.attach(A, true, 0);
        assert!(lease.sweep(u64::MAX - 1).is_empty());
    }

    #[test]
    fn a_takeover_after_a_later_beat_counts_from_the_takeover() {
        let mut lease = three();
        lease.beat(A, 50_000);
        // The taker's time is its own: 30 000 ms after the takeover, whatever the previous holder did.
        lease.takeover(B, 20_000).expect("takeover");
        assert!(lease.sweep(49_999).is_empty());
        assert_eq!(lease.sweep(50_000).len(), 3);
    }

    #[test]
    fn the_deadline_is_the_last_sign_of_life_plus_the_timeout_while_somebody_holds() {
        let mut lease = Lease::new(100);
        assert_eq!(lease.deadline(), None);
        lease.attach(A, true, 40);
        assert_eq!(lease.deadline(), Some(140));
        lease.beat(A, 90);
        assert_eq!(lease.deadline(), Some(190));
        lease.sweep(190);
        assert_eq!(lease.deadline(), None);
        assert_eq!(Lease::new(u64::MAX).deadline(), None, "nobody holds it");
    }

    #[test]
    fn the_timeout_is_a_setting_of_the_lease() {
        let mut lease = Lease::new(100);
        lease.attach(A, true, 0);
        lease.attach(B, false, 0);
        assert!(lease.sweep(99).is_empty());
        assert_eq!(lease.sweep(100), vec![(A, Free), (B, Free)]);
        assert_eq!(WRITER_TIMEOUT_MS, 30_000);
        assert_eq!(BEAT_INTERVAL_MS * 3, WRITER_TIMEOUT_MS);
    }

    #[test]
    fn the_words_are_the_ones_of_the_labels() {
        assert_eq!([Writing.word(), Watching.word(), Free.word()], ["writing", "watching", "free"]);
    }
}
