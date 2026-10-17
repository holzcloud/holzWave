//! The stream state machine: what to do with each event of a model call and with each tick of the clock.
//!
//! It calls nothing foreign and reads no clock: every method takes the time as an argument, so the rules (snapshot to
//! delta, one cancel, the grace period, the idle timeout, the terminal event) are tested with a synthetic clock on every
//! host. The loop of the system backend only does what [`Next`] says.

use std::time::{Duration, Instant};

use crate::backend::{AppleError, MAX_INPUT_BYTES};

/// Something that happened to the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// The whole reply so far (cumulative, not a delta).
    Snapshot(String),
    /// The reply ended cleanly. Terminal.
    End,
    /// The call ended with this error. Terminal.
    Error(AppleError),
    /// A snapshot was larger than the bound and was not copied.
    Oversize,
    /// A snapshot could not be read (invalid text). The call is not over: the model goes on until it is cancelled.
    Malformed,
}

/// What the loop does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Next {
    /// Nothing; wait for the next event or tick.
    Wait,
    /// Give this text to the consumer.
    Deliver(String),
    /// Ask the model to stop. Said once per call.
    Cancel,
    /// The call is over with this result.
    Done(Result<(), AppleError>),
}

/// The bounds of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PumpLimits {
    /// A stream that stays quiet this long is a timeout.
    pub idle: Duration,
    /// After a cancel is asked, the model has this long to end the call; then the call is abandoned.
    pub cancel_grace: Duration,
    /// The longest reply, in bytes.
    pub max_reply_bytes: usize,
}

impl PumpLimits {
    /// 60 seconds idle, a 250 ms grace, a 64 KiB reply.
    pub(crate) fn standard() -> Self {
        Self { idle: Duration::from_secs(60), cancel_grace: Duration::from_millis(250), max_reply_bytes: MAX_INPUT_BYTES }
    }
}

/// The state of one call.
pub(crate) struct Pump {
    limits: PumpLimits,
    /// The reply delivered so far. Append only.
    delivered: String,
    last_activity: Instant,
    /// When a cancel (or a fault, which asks for one) was recorded.
    cancel_at: Option<Instant>,
    cancel_sent: bool,
    stop: bool,
    fault: Option<AppleError>,
    done: Option<Result<(), AppleError>>,
    terminal_seen: bool,
    abandoned: bool,
}

impl Pump {
    /// A pump for a call that starts at `now`.
    pub(crate) fn new(limits: PumpLimits, now: Instant) -> Self {
        Self {
            limits,
            delivered: String::new(),
            last_activity: now,
            cancel_at: None,
            cancel_sent: false,
            stop: false,
            fault: None,
            done: None,
            terminal_seen: false,
            abandoned: false,
        }
    }

    /// The consumer answered stop: acts like a requested cancel, recorded at the next event or tick.
    pub(crate) fn stop_requested(&mut self) {
        self.stop = true;
    }

    /// Whether the model's own end or error event was seen. The end state of a call, read by the tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn terminal_seen(&self) -> bool {
        self.terminal_seen
    }

    /// Whether the call was given up without a terminal event. The end state of a call, read by the tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn abandoned(&self) -> bool {
        self.abandoned
    }

    fn finished(&self) -> Option<Next> {
        self.done.clone().map(Next::Done)
    }

    /// Records a cancel request at `now` when there is none and returns whether this is the first one.
    fn request_cancel(&mut self, now: Instant) -> bool {
        if self.cancel_at.is_none() {
            self.cancel_at = Some(now);
            true
        } else {
            false
        }
    }

    /// Notes a fault: the first one wins, and it asks for a cancel.
    fn fault(&mut self, kind: AppleError, now: Instant) -> Next {
        if self.fault.is_none() {
            self.fault = Some(kind);
        }
        self.request_cancel(now);
        self.send_cancel()
    }

    /// `Cancel` the first time, `Wait` after that: a cancel that a stop request recorded goes out with the next event or
    /// tick, and one that is out already is not repeated.
    fn send_cancel(&mut self) -> Next {
        if self.cancel_sent {
            Next::Wait
        } else {
            self.cancel_sent = true;
            Next::Cancel
        }
    }

    fn absorb_stop(&mut self, now: Instant) {
        if self.stop {
            self.request_cancel(now);
        }
    }

    fn pending(&self) -> bool {
        self.cancel_at.is_some() || self.fault.is_some()
    }

    fn end(&mut self, model_result: Result<(), AppleError>) -> Next {
        let result = if let Some(fault) = self.fault {
            Err(fault)
        } else if self.cancel_at.is_some() {
            Err(AppleError::Cancelled)
        } else {
            model_result
        };
        self.terminal_seen = true;
        self.done = Some(result.clone());
        Next::Done(result)
    }

    /// Takes one event at `now`.
    pub(crate) fn event(&mut self, event: Event, now: Instant) -> Next {
        if let Some(done) = self.finished() {
            return done;
        }
        self.absorb_stop(now);
        match event {
            Event::End => self.end(Ok(())),
            Event::Error(kind) => self.end(Err(kind)),
            Event::Oversize if self.pending() => self.send_cancel(),
            Event::Oversize => self.fault(AppleError::ReplyTooLarge, now),
            Event::Malformed if self.pending() => self.send_cancel(),
            Event::Malformed => self.fault(AppleError::Other, now),
            Event::Snapshot(_) if self.pending() => self.send_cancel(),
            Event::Snapshot(text) => self.snapshot(text, now),
        }
    }

    fn snapshot(&mut self, text: String, now: Instant) -> Next {
        if text.len() > self.limits.max_reply_bytes {
            return self.fault(AppleError::ReplyTooLarge, now);
        }
        if !text.starts_with(self.delivered.as_str()) {
            return self.fault(AppleError::Inconsistent, now);
        }
        self.last_activity = now;
        if text.len() == self.delivered.len() {
            return Next::Wait;
        }
        // `delivered` is a whole string and a prefix of `text`, so the cut is on a character boundary.
        let delta = text[self.delivered.len()..].to_owned();
        self.delivered.push_str(&delta);
        Next::Deliver(delta)
    }

    /// The clock moved to `now`. `cancel_requested` is the caller's token.
    pub(crate) fn tick(&mut self, now: Instant, cancel_requested: bool) -> Next {
        if let Some(done) = self.finished() {
            return done;
        }
        if cancel_requested {
            self.stop = true;
        }
        self.absorb_stop(now);
        if self.cancel_at.is_none() && now.saturating_duration_since(self.last_activity) >= self.limits.idle {
            return self.fault(AppleError::Timeout, now);
        }
        if let Some(at) = self.cancel_at {
            if now.saturating_duration_since(at) >= self.limits.cancel_grace && self.cancel_sent {
                let result = Err(self.fault.unwrap_or(AppleError::Cancelled));
                self.abandoned = true;
                self.done = Some(result.clone());
                return Next::Done(result);
            }
            return self.send_cancel();
        }
        Next::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pump() -> (Pump, Instant) {
        let t0 = Instant::now();
        (Pump::new(PumpLimits::standard(), t0), t0)
    }

    fn snap(text: &str) -> Event {
        Event::Snapshot(text.to_owned())
    }

    #[test]
    fn a_growing_snapshot_becomes_one_delta_each_time() {
        let (mut pump, t0) = pump();
        assert_eq!(pump.event(snap("Hel"), t0), Next::Deliver("Hel".to_owned()));
        assert_eq!(pump.event(snap("Hello"), t0), Next::Deliver("lo".to_owned()));
        assert_eq!(pump.event(snap("Hello, w\u{f6}rld"), t0), Next::Deliver(", w\u{f6}rld".to_owned()));
        assert_eq!(pump.event(Event::End, t0), Next::Done(Ok(())));
        assert!(pump.terminal_seen() && !pump.abandoned());
    }

    #[test]
    fn an_unchanged_snapshot_is_no_event() {
        let (mut pump, t0) = pump();
        assert_eq!(pump.event(snap("abc"), t0), Next::Deliver("abc".to_owned()));
        assert_eq!(pump.event(snap("abc"), t0), Next::Wait);
    }

    #[test]
    fn a_snapshot_that_rewrites_earlier_text_is_a_fault_and_asks_for_a_cancel() {
        for rewrite in ["abx", "ab", "xyz!", ""] {
            let (mut pump, t0) = pump();
            assert_eq!(pump.event(snap("abc"), t0), Next::Deliver("abc".to_owned()));
            assert_eq!(pump.event(snap(rewrite), t0), Next::Cancel, "{rewrite:?}");
            // Later snapshots are ignored and the cancel is not repeated.
            assert_eq!(pump.event(snap("abcdef"), t0), Next::Wait);
            assert_eq!(pump.event(Event::End, t0), Next::Done(Err(AppleError::Inconsistent)));
        }
    }

    #[test]
    fn a_reply_over_the_limit_is_a_fault_and_asks_for_a_cancel() {
        let limits = PumpLimits { max_reply_bytes: 8, ..PumpLimits::standard() };
        let t0 = Instant::now();
        let mut pump = Pump::new(limits, t0);
        assert_eq!(pump.event(snap("12345678"), t0), Next::Deliver("12345678".to_owned()));
        assert_eq!(pump.event(snap("123456789"), t0), Next::Cancel);
        assert_eq!(pump.event(Event::Error(AppleError::Other), t0), Next::Done(Err(AppleError::ReplyTooLarge)));

        // An oversize event from the trampoline is the same fault.
        let mut pump = Pump::new(limits, t0);
        assert_eq!(pump.event(Event::Oversize, t0), Next::Cancel);
        assert_eq!(pump.event(Event::Oversize, t0), Next::Wait);
        assert_eq!(pump.event(Event::End, t0), Next::Done(Err(AppleError::ReplyTooLarge)));
    }

    #[test]
    fn end_without_a_fault_is_ok() {
        let (mut pump, t0) = pump();
        assert_eq!(pump.event(Event::End, t0), Next::Done(Ok(())));
        assert!(pump.terminal_seen());
    }

    #[test]
    fn an_error_event_is_the_result_unless_a_cancel_was_requested() {
        let (mut pump, t0) = pump();
        assert_eq!(pump.event(Event::Error(AppleError::Guardrail), t0), Next::Done(Err(AppleError::Guardrail)));

        let (mut pump, t0) = self::pump();
        assert_eq!(pump.tick(t0, true), Next::Cancel);
        assert_eq!(pump.event(Event::Error(AppleError::Guardrail), t0), Next::Done(Err(AppleError::Cancelled)));

        // A clean end after a cancel request is still a cancelled call.
        let (mut pump, t0) = self::pump();
        assert_eq!(pump.tick(t0, true), Next::Cancel);
        assert_eq!(pump.event(Event::End, t0), Next::Done(Err(AppleError::Cancelled)));
    }

    #[test]
    fn a_cancel_is_sent_once_and_the_result_is_cancelled() {
        let (mut pump, t0) = pump();
        assert_eq!(pump.event(snap("a"), t0), Next::Deliver("a".to_owned()));
        assert_eq!(pump.tick(t0, true), Next::Cancel);
        assert_eq!(pump.tick(t0, true), Next::Wait);
        assert_eq!(pump.tick(t0 + Duration::from_millis(10), true), Next::Wait);
        // A snapshot that races with the cancel is not delivered.
        assert_eq!(pump.event(snap("ab"), t0 + Duration::from_millis(10)), Next::Wait);
        assert_eq!(pump.event(Event::Error(AppleError::Cancelled), t0 + Duration::from_millis(20)), Next::Done(Err(AppleError::Cancelled)));
        assert!(pump.terminal_seen() && !pump.abandoned());
    }

    #[test]
    fn no_answer_within_the_grace_abandons_the_call() {
        let (mut pump, t0) = pump();
        assert_eq!(pump.tick(t0, true), Next::Cancel);
        assert_eq!(pump.tick(t0 + Duration::from_millis(249), true), Next::Wait);
        assert_eq!(pump.tick(t0 + Duration::from_millis(250), true), Next::Done(Err(AppleError::Cancelled)));
        assert!(pump.abandoned());
        assert!(!pump.terminal_seen());
    }

    #[test]
    fn an_idle_stream_times_out_and_asks_for_a_cancel() {
        let (mut pump, t0) = pump();
        assert_eq!(pump.tick(t0 + Duration::from_secs(59), false), Next::Wait);
        assert_eq!(pump.event(snap("a"), t0 + Duration::from_secs(59)), Next::Deliver("a".to_owned()));
        // The delta reset the idle clock.
        assert_eq!(pump.tick(t0 + Duration::from_secs(118), false), Next::Wait);
        assert_eq!(pump.tick(t0 + Duration::from_secs(119), false), Next::Cancel);
        assert_eq!(pump.tick(t0 + Duration::from_secs(119), false), Next::Wait);
        assert_eq!(pump.tick(t0 + Duration::from_millis(119_000 + 250), false), Next::Done(Err(AppleError::Timeout)));
        assert!(pump.abandoned());
    }

    #[test]
    fn events_after_the_terminal_are_ignored() {
        let (mut pump, t0) = pump();
        assert_eq!(pump.event(snap("a"), t0), Next::Deliver("a".to_owned()));
        assert_eq!(pump.event(Event::End, t0), Next::Done(Ok(())));
        assert_eq!(pump.event(snap("ab"), t0), Next::Done(Ok(())));
        assert_eq!(pump.event(Event::Error(AppleError::Other), t0), Next::Done(Ok(())));
        assert_eq!(pump.tick(t0 + Duration::from_secs(600), true), Next::Done(Ok(())));
    }

    #[test]
    fn a_sink_stop_acts_like_a_cancel() {
        let (mut pump, t0) = pump();
        assert_eq!(pump.event(snap("a"), t0), Next::Deliver("a".to_owned()));
        pump.stop_requested();
        assert_eq!(pump.tick(t0, false), Next::Cancel);
        assert_eq!(pump.tick(t0, false), Next::Wait);
        assert_eq!(pump.tick(t0 + Duration::from_millis(250), false), Next::Done(Err(AppleError::Cancelled)));
        assert!(pump.abandoned());

        // With a terminal event inside the grace the call ends as cancelled and is not abandoned.
        let (mut pump, t0) = self::pump();
        pump.stop_requested();
        assert_eq!(pump.event(snap("a"), t0), Next::Cancel);
        assert_eq!(pump.event(Event::End, t0), Next::Done(Err(AppleError::Cancelled)));
        assert!(!pump.abandoned() && pump.terminal_seen());
    }
}
