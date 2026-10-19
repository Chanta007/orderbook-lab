//! Reading the daemon's own account of itself out of its log.
//!
//! The status view is asked for the queues, the consumers and the back pressure.
//! Those live inside the feed daemon, and the daemon already publishes them, once a
//! second, on the structured log lines described in `docs/design/observability.md`
//! §4.2:
//!
//! ```text
//! t=… op=report published=2400000 applied=… ignored=0 crossed=0 evicted=0 dropped=0 symbols=1000 connections=1 invalid=0 bytes=… rate=7
//! t=… op=latency stage1=… stage2=n=20 min=1.2us p50=1.7us p90=8.7us p99=20.5us max=19.3us stamped=0 unstamped=20 implausible=0
//! subscriber 0: delivered=2400000 evicted=0 dropped=0 high_water=2041 state=nominal | stage3: n=… p99=165.5us
//! ```
//!
//! Reading them is why the status view needs no change to the wire format — and the
//! wire format is shared by three implementations, so a change to it lands as one
//! commit across all of them (`docs/VERIFY.md` rule 2).
//!
//! # What this parses, and what it refuses to
//!
//! A report block starts at the last line carrying `op=report`; the latency line and
//! the subscriber lines that follow it belong to that block. Within a line, values
//! are taken verbatim: `p50=1.7us` stays `1.7us`, because the daemon already chose
//! the unit and a second guess at it would be a second place for the two to
//! disagree. A field that is not there is `None` and renders as `-`, never as zero:
//! "no samples" and "zero samples" are different claims.

/// One subscriber's counters, as the daemon reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Subscriber {
    /// Which subscriber, in the order the daemon registered them.
    pub index: u32,
    /// Events delivered to it.
    pub delivered: Option<u64>,
    /// Events evicted from its queue.
    pub evicted: Option<u64>,
    /// Events dropped instead of queued.
    pub dropped: Option<u64>,
    /// The deepest its queue has been.
    pub high_water: Option<u64>,
    /// The pressure state it is in: `nominal`, `elevated`, `saturated`.
    pub state: Option<String>,
    /// Stage 3 latency, as the daemon formatted it.
    pub stage3_p50: Option<String>,
    /// Stage 3 latency at the 99th percentile, as the daemon formatted it.
    pub stage3_p99: Option<String>,
}

/// The daemon's newest report, as far as its log says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonReport {
    /// Records it has published to the ring.
    pub published: Option<u64>,
    /// Records its cache applied.
    pub applied: Option<u64>,
    /// Records it could not apply.
    pub ignored: Option<u64>,
    /// Times the book had a bid at or above an ask.
    pub crossed: Option<u64>,
    /// Events evicted from a subscriber's queue.
    pub evicted: Option<u64>,
    /// Events dropped rather than queued.
    pub dropped: Option<u64>,
    /// Symbols in its cache.
    pub symbols: Option<u64>,
    /// Live connections.
    pub connections: Option<u64>,
    /// Frames it rejected.
    pub invalid: Option<u64>,
    /// Records per second.
    pub rate: Option<u64>,
    /// Records that carried a sender's stamp.
    pub stamped: Option<u64>,
    /// Records that did not, so stage 1 is not measured for them.
    pub unstamped: Option<u64>,
    /// Stamps that cannot be a duration on this host.
    pub implausible: Option<u64>,
    /// Stage 2 latency at the median, as the daemon formatted it.
    pub stage2_p50: Option<String>,
    /// Stage 2 latency at the 99th percentile, as the daemon formatted it.
    pub stage2_p99: Option<String>,
    /// One entry per subscriber.
    pub subscribers: Vec<Subscriber>,
}

impl DaemonReport {
    /// Whether anything at all was read. A block with no field in it is not a report.
    pub fn is_empty(&self) -> bool {
        self.published.is_none()
            && self.applied.is_none()
            && self.rate.is_none()
            && self.stamped.is_none()
            && self.unstamped.is_none()
            && self.implausible.is_none()
            && self.stage2_p50.is_none()
            && self.stage2_p99.is_none()
            && self.subscribers.is_empty()
    }

    /// Whether any counter says data was lost.
    ///
    /// The frame colours on this: a non-zero `dropped` or `evicted` means a
    /// consumer did not see records it should have, which is the one thing on this
    /// panel that is a fault rather than a reading.
    pub fn lost(&self) -> u64 {
        // A subscriber's counters are the ones that say a consumer lost records:
        // the daemon's own `dropped` counts what it did with records it could not
        // hand to anyone, which is a different question.
        self.dropped.unwrap_or(0)
            + self.evicted.unwrap_or(0)
            + self
                .subscribers
                .iter()
                .map(|s| s.dropped.unwrap_or(0) + s.evicted.unwrap_or(0))
                .sum::<u64>()
    }
}

/// Parse the newest report out of a log's tail, or `None` when it has none.
///
/// `tail` is the end of the daemon's log — a few kilobytes is plenty, and the caller
/// decides how much to read.
pub fn parse_report(tail: &str) -> Option<DaemonReport> {
    let lines: Vec<&str> = tail.lines().collect();
    // A block begins at the report line, or at the latency line when the report
    // line has been rotated out of the tail: both belong to one second's block, and
    // refusing to read the second because the first is missing would report nothing
    // for a daemon that is reporting fine.
    let start = lines
        .iter()
        .rposition(|line| line.contains("op=report"))
        .or_else(|| lines.iter().rposition(|line| line.contains("op=latency")))?;
    let mut report = DaemonReport::default();
    for line in &lines[start..] {
        if line.contains("op=report") {
            report.published = number(line, "published");
            report.applied = number(line, "applied");
            report.ignored = number(line, "ignored");
            report.crossed = number(line, "crossed");
            report.evicted = number(line, "evicted");
            report.dropped = number(line, "dropped");
            report.symbols = number(line, "symbols");
            report.connections = number(line, "connections");
            report.invalid = number(line, "invalid");
            report.rate = number(line, "rate");
        } else if line.contains("op=latency") {
            report.stamped = number(line, "stamped");
            report.unstamped = number(line, "unstamped");
            report.implausible = number(line, "implausible");
            // Only the stage 2 group. The line carries a stage 1 group with the
            // same key names in it, so a search of the whole line would read the
            // wrong one of the two.
            if let Some((_, stage2)) = line.split_once("stage2=") {
                report.stage2_p50 = text(stage2, "p50");
                report.stage2_p99 = text(stage2, "p99");
            }
        } else if let Some(rest) = line.trim_start().strip_prefix("subscriber ") {
            if let Some(subscriber) = parse_subscriber(rest) {
                report.subscribers.push(subscriber);
            }
        }
    }
    if report.is_empty() {
        return None;
    }
    Some(report)
}

/// `0: delivered=… | stage3: …`.
fn parse_subscriber(rest: &str) -> Option<Subscriber> {
    let (index, body) = rest.split_once(':')?;
    let index: u32 = index.trim().parse().ok()?;
    let (counters, stage3) = match body.split_once('|') {
        Some((a, b)) => (a, b),
        None => (body, ""),
    };
    let (stage3, _) = match stage3.split_once("stage3:") {
        Some((_, after)) => (after, ""),
        None => ("", ""),
    };
    let mut out = Subscriber {
        index,
        ..Subscriber::default()
    };
    out.delivered = number(counters, "delivered");
    out.evicted = number(counters, "evicted");
    out.dropped = number(counters, "dropped");
    out.high_water = number(counters, "high_water");
    out.state = text(counters, "state");
    out.stage3_p50 = text(stage3, "p50");
    out.stage3_p99 = text(stage3, "p99");
    Some(out)
}

/// The value of `key=…` in a line, when the key starts a word.
///
/// The boundary check matters: `p99=` must not match inside `p99.9=`, and
/// `dropped=` must not match `total_dropped=`.
fn value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    let mut from = 0;
    while let Some(at) = line[from..].find(&needle) {
        let start = from + at;
        let before_ok = start == 0
            || line[..start]
                .chars()
                .next_back()
                // `=` is here because the daemon writes `stage2=n=20`: one key's
                // value is the next key.
                .is_some_and(|c| c.is_whitespace() || c == '|' || c == ':' || c == '(' || c == '=');
        if before_ok {
            let value_start = start + needle.len();
            let rest = &line[value_start..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '|')
                .unwrap_or(rest.len());
            return Some(&rest[..end]);
        }
        from = start + needle.len();
    }
    None
}

fn number(line: &str, key: &str) -> Option<u64> {
    value(line, key)?.parse().ok()
}

fn text(line: &str, key: &str) -> Option<String> {
    let v = value(line, key)?;
    if v.is_empty() || v == "-" {
        return None;
    }
    Some(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a real block, taken from docs/design/observability.md §4.2.
    const TAIL: &str = "\
t=1932789939555958 level=info component=feedd op=report published=20 applied=20 ignored=0 crossed=0 evicted=0 dropped=0 symbols=1 connections=1 invalid=0 bytes=1440 rate=7
t=1932789939556000 level=info component=feedd op=latency stage1=(no samples) stage2=n=20 min=1.2us p50=1.7us p90=8.7us p99=20.5us max=19.3us stamped=0 unstamped=20 implausible=0
subscriber 0: delivered=20 evicted=0 dropped=0 high_water=8 state=nominal | stage3: n=20 min=24.7us p50=133.4us p99=205.4us max=201.3us
subscriber 1: delivered=20 evicted=1 dropped=2 high_water=64 state=saturated | stage3: n=20 min=26.2us p50=140.0us p99=260.0us max=199.6us
";

    #[test]
    fn a_real_block_parses_into_its_fields() {
        let report = parse_report(TAIL).expect("a report");
        assert_eq!(report.published, Some(20));
        assert_eq!(report.applied, Some(20));
        assert_eq!(report.crossed, Some(0));
        assert_eq!(report.rate, Some(7));
        assert_eq!(report.symbols, Some(1));
        assert_eq!(report.unstamped, Some(20));
        assert_eq!(report.implausible, Some(0));
        assert_eq!(report.stage2_p50.as_deref(), Some("1.7us"));
        assert_eq!(report.stage2_p99.as_deref(), Some("20.5us"));
    }

    #[test]
    fn the_subscribers_are_kept_apart_with_their_states() {
        let report = parse_report(TAIL).expect("a report");
        assert_eq!(report.subscribers.len(), 2);
        let first = &report.subscribers[0];
        assert_eq!(first.index, 0);
        assert_eq!(first.delivered, Some(20));
        assert_eq!(first.high_water, Some(8));
        assert_eq!(first.state.as_deref(), Some("nominal"));
        assert_eq!(first.stage3_p99.as_deref(), Some("205.4us"));
        let second = &report.subscribers[1];
        assert_eq!(second.index, 1);
        assert_eq!(second.evicted, Some(1));
        assert_eq!(second.state.as_deref(), Some("saturated"));
        assert_eq!(report.lost(), 3, "one evicted and two dropped");
    }

    #[test]
    fn stage_two_is_not_confused_with_the_stage_one_group_before_it() {
        // The line carries stage1=(no samples) and then stage2=n=… p50=…. A search
        // of the whole line would find the stage 1 group's keys, which are absent
        // here, or stage 1's numbers in a line that has both.
        let line = "t=1 op=latency stage1=n=5 min=1.0us p50=9.9us p99=99.9us stage2=n=20 min=1.2us p50=1.7us p99=20.5us";
        let report = parse_report(line).expect("a report");
        assert_eq!(report.stage2_p50.as_deref(), Some("1.7us"));
        assert_eq!(report.stage2_p99.as_deref(), Some("20.5us"));
    }

    #[test]
    fn the_newest_block_is_the_one_reported() {
        let older = "t=1 op=report published=5 applied=5 rate=1\n";
        let newest = "t=2 op=report published=900 applied=899 rate=42\n";
        let report = parse_report(&format!("{older}{newest}")).expect("a report");
        assert_eq!(report.published, Some(900), "the last block wins");
        assert_eq!(report.rate, Some(42));
    }

    #[test]
    fn a_log_with_no_report_is_not_a_report() {
        assert!(parse_report("").is_none());
        assert!(parse_report("t=1 level=info op=stop published=20\n").is_none());
        assert!(parse_report("just a line\n").is_none());
    }

    #[test]
    fn an_absent_field_is_none_and_never_zero() {
        // "no samples" and "zero samples" are different claims, and the frame
        // renders a None as `-`.
        let report = parse_report("t=1 op=report published=1 rate=1\n").expect("a report");
        assert_eq!(report.applied, None);
        assert_eq!(report.stage2_p50, None);
        assert_eq!(report.dropped, None, "an absent counter is not a zero");
        assert_eq!(report.lost(), 0, "and it does not inflate the loss count");
    }

    #[test]
    fn a_key_that_is_a_prefix_of_another_key_does_not_match_it() {
        let line = "t=1 op=latency stage2=n=1 p99.9=250.0us p99=20.5us";
        let report = parse_report(line).expect("a report");
        assert_eq!(report.stage2_p99.as_deref(), Some("20.5us"));
        assert_eq!(value(line, "n"), Some("1"));
        assert_eq!(value(line, "total_dropped"), None);
    }

    #[test]
    fn a_subscriber_line_without_a_stage_three_group_still_parses() {
        let report = parse_report("t=1 op=report published=1\nsubscriber 3: delivered=7\n")
            .expect("a report");
        assert_eq!(report.subscribers.len(), 1);
        assert_eq!(report.subscribers[0].delivered, Some(7));
        assert_eq!(report.subscribers[0].stage3_p99, None);
        assert_eq!(report.subscribers[0].state, None);
    }
}
