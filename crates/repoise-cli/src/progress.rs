//! Human progress rendering for `repoise index`.
//!
//! Core services emit structured phase/counter events
//! (`repoise_core::progress`); this module owns all terminal rendering.
//! Human mode writes progress to stdout alongside the final summary;
//! `--json` mode keeps stdout as the single final JSON document and sends
//! progress to stderr instead. Rendering is throttled (at most one line per
//! 100 ms within a phase; phase changes always render) and never emits
//! per-file noise. A phase's final line is printed exactly once: a repeated
//! completion report (for example a trailing callback) or a `phase_finished`
//! that would only repeat the last line is skipped. On a TTY the current
//! phase line is rewritten in place and terminated when the phase finishes
//! (or via `finish`), so the final summary or an error always starts on a
//! new line; otherwise new lines are appended.

use std::io::{self, IsTerminal, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use repoise_core::progress::{IndexPhase, IndexProgressReporter};

/// Minimum interval between progress lines within one phase.
const UPDATE_INTERVAL: Duration = Duration::from_millis(100);
/// Minimum completed units before a per-unit ETA is shown (below this a few
/// slow units skew the estimate too much to be honest).
pub(crate) const MIN_UNITS_FOR_ETA: u64 = 8;
/// Minimum observed phase time before an ETA is shown.
pub(crate) const MIN_TIME_FOR_ETA: Duration = Duration::from_millis(100);

/// Renders index progress to stdout, or stderr in `--json` mode.
pub struct ProgressReporter {
    inner: Mutex<Inner>,
}

struct Inner {
    /// When true, progress lines go to stderr (JSON mode).
    stderr: bool,
    /// Rewrite the current line in place (terminal output only).
    in_place: bool,
    /// Build start (elapsed is shown on every line).
    start: Instant,
    /// Current phase, if any.
    phase: Option<IndexPhase>,
    /// When the current phase started (ETA baseline).
    phase_started: Instant,
    /// Completed units in the current phase.
    done: u64,
    /// Latest reliable denominator for the current phase.
    total: Option<u64>,
    /// Last render time (throttle).
    last_render: Instant,
    /// Last in-place line (padding calculation).
    last_line: String,
    /// Text of the last line actually written (either mode); identical
    /// re-renders are skipped.
    last_rendered: String,
    /// True once this phase's completion line (done == total) has been
    /// rendered, so a repeated completion report or the phase end does not
    /// print it again.
    completion_rendered: bool,
    /// (done, time) of the first progress report in the current phase. The
    /// ETA rate is measured from this baseline so units credited at phase
    /// start (embedding cache hits) do not skew it.
    eta_base: Option<(u64, Instant)>,
}

impl ProgressReporter {
    /// Creates a reporter; `stderr` sends progress to stderr (JSON mode).
    pub fn new(stderr: bool) -> Self {
        let in_place = if stderr {
            io::stderr().is_terminal()
        } else {
            io::stdout().is_terminal()
        };
        Self {
            inner: Mutex::new(Inner {
                stderr,
                in_place,
                start: Instant::now(),
                phase: None,
                phase_started: Instant::now(),
                done: 0,
                total: None,
                last_render: Instant::now(),
                last_line: String::new(),
                last_rendered: String::new(),
                completion_rendered: false,
                eta_base: None,
            }),
        }
    }

    /// Elapsed time since the reporter was created.
    pub fn elapsed(&self) -> Duration {
        self.inner.lock().unwrap().start.elapsed()
    }
}

impl IndexProgressReporter for ProgressReporter {
    fn phase_started(&self, phase: IndexPhase, total: Option<u64>) {
        self.emit(|state, stream| state.on_phase_started(stream, phase, total));
    }

    fn progress(&self, phase: IndexPhase, done: u64, total: u64) {
        self.emit(|state, stream| state.on_progress(stream, phase, done, total));
    }

    fn phase_finished(&self, phase: IndexPhase) {
        self.emit(|state, stream| state.on_phase_finished(stream, phase));
    }
}

impl ProgressReporter {
    /// Runs `action` against the active stream, then flushes it.
    fn emit(&self, action: impl FnOnce(&mut Inner, &mut dyn Write)) {
        let mut state = self.inner.lock().unwrap();
        if state.stderr {
            action(&mut state, &mut io::stderr());
            let _ = io::stderr().flush();
        } else {
            action(&mut state, &mut io::stdout());
            let _ = io::stdout().flush();
        }
    }

    /// Terminates any unterminated in-place progress line (idempotent) so
    /// the final summary or an error prints on a fresh line. No-op when
    /// lines are appended or no in-place line is open.
    pub fn finish(&self) {
        self.emit(|state, stream| state.finish_line(stream));
    }
}

impl Inner {
    /// Starts a new phase, resetting its counters and rendering its line.
    fn on_phase_started(&mut self, stream: &mut dyn Write, phase: IndexPhase, total: Option<u64>) {
        self.phase = Some(phase);
        self.phase_started = Instant::now();
        self.done = 0;
        self.total = total;
        self.eta_base = None;
        self.completion_rendered = false;
        self.last_render = Instant::now();
        self.render(stream);
    }

    /// Applies one in-phase counter update (completed units of `total`).
    fn on_progress(&mut self, stream: &mut dyn Write, phase: IndexPhase, done: u64, total: u64) {
        self.phase = Some(phase);
        self.done = done;
        self.total = Some(total);
        // A repeated completion report (for example a trailing callback
        // after the final batch) must not print the final line again.
        if done == total && self.completion_rendered {
            return;
        }
        let now = Instant::now();
        // The phase's first report is the ETA baseline: units credited
        // before it (embedding cache hits) must not skew the rate.
        if self.eta_base.is_none() {
            self.eta_base = Some((done, now));
        }
        // Completion always renders; otherwise stay within the throttle.
        if done == total || now.duration_since(self.last_render) >= UPDATE_INTERVAL {
            self.last_render = now;
            self.render(stream);
            self.completion_rendered = done == total;
        }
    }

    /// Ends the phase: prints the final line unless the completion line was
    /// already printed, then terminates the in-place line so the next phase
    /// (or the summary) starts on a new line.
    fn on_phase_finished(&mut self, stream: &mut dyn Write, phase: IndexPhase) {
        self.phase = Some(phase);
        self.last_render = Instant::now();
        if !self.completion_rendered {
            self.render(stream);
        }
        self.finish_line(stream);
    }

    fn render(&mut self, stream: &mut dyn Write) {
        let line = self.line();
        // Never repeat a line already on screen (a phase end or a trailing
        // report within the same second would otherwise print it twice in
        // append mode).
        if line.is_empty() || line == self.last_rendered {
            return;
        }
        if self.in_place {
            let pad = " ".repeat(self.last_line.len().saturating_sub(line.len()));
            let _ = write!(stream, "\r{line}{pad}");
        } else {
            let _ = writeln!(stream, "{line}");
        }
        self.last_rendered = line.clone();
        self.last_line = if self.in_place { line } else { String::new() };
    }

    /// Terminates an unterminated in-place line so subsequent output (the
    /// final summary or an error) starts on a new line. Idempotent: append
    /// mode lines already carry their own newline.
    fn finish_line(&mut self, stream: &mut dyn Write) {
        if self.in_place && !self.last_line.is_empty() {
            let _ = writeln!(stream);
            self.last_line = String::new();
        }
    }

    /// The current phase line: phase, counts and percent when a reliable
    /// denominator exists, elapsed time, and an approximate ETA only when
    /// the observed rate is trustworthy enough.
    fn line(&self) -> String {
        let Some(phase) = self.phase else {
            return String::new();
        };
        let elapsed = format_duration(self.start.elapsed());
        match self.total {
            Some(total) if total > 0 => {
                let percent = self.done * 100 / total;
                let mut line = format!(
                    "index: {} {}/{} {} ({}%) [{}]",
                    phase.name(),
                    self.done,
                    total,
                    unit_for(phase),
                    percent,
                    elapsed
                );
                if let Some(eta) = self.eta_seconds() {
                    line.push_str(&format!(" ~{eta}s left"));
                }
                line
            }
            _ => format!("index: {} working [{}]", phase.name(), elapsed),
        }
    }

    /// Approximate seconds remaining, or `None` when the estimate would be
    /// misleading. The rate is measured from the phase's first progress
    /// report: units credited before it (embedding cache hits reported at
    /// phase start) must not count as instantly finished work.
    fn eta_seconds(&self) -> Option<u64> {
        let (base_done, base_time) = self.eta_base?;
        eta_seconds_from(self.done, self.total?, Some(base_done), base_time.elapsed())
    }
}

/// Pure ETA arithmetic: `None` when the estimate would be misleading
/// (no first-report baseline yet, too few units completed since it, too
/// little observed time, or a sub-second remainder).
pub(crate) fn eta_seconds_from(
    done: u64,
    total: u64,
    base_done: Option<u64>,
    observed: Duration,
) -> Option<u64> {
    let base = base_done?;
    let delta = done.saturating_sub(base);
    if delta < MIN_UNITS_FOR_ETA || total <= done {
        return None;
    }
    let observed_secs = observed.as_secs_f64();
    if observed < MIN_TIME_FOR_ETA || observed_secs <= 0.0 {
        return None;
    }
    let remaining_secs = (total - done) as f64 / (delta as f64 / observed_secs);
    if remaining_secs < 1.0 {
        return None;
    }
    Some(remaining_secs.round().max(1.0) as u64)
}

/// The work unit name for a phase's counts.
fn unit_for(phase: IndexPhase) -> &'static str {
    match phase {
        IndexPhase::Discovery => "candidates",
        IndexPhase::Reconcile => "files",
        IndexPhase::Embedding => "chunks",
        IndexPhase::History => "items",
        IndexPhase::Publication => "steps",
    }
}

/// Human duration: seconds with one decimal under a minute, minutes beyond.
pub fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let minutes = (secs / 60.0) as u64;
        let remainder = (secs - f64::from(minutes as u32) * 60.0) as u64;
        format!("{minutes}m {remainder:02}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn format_duration_switches_units() {
        assert_eq!(format_duration(Duration::from_millis(250)), "0.2s");
        assert_eq!(format_duration(Duration::from_secs(59)), "59.0s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 05s");
    }

    /// The ETA rate must be measured from the phase's first progress
    /// report, so cache hits credited at phase start do not make the ETA
    /// look faster than real work.
    #[test]
    fn eta_baseline_excludes_units_before_first_report() {
        // Reviewer scenario: 9,000 of 10,000 chunks are cache hits reported
        // as the first progress event; 50 s later the provider has done 500
        // more. The rate is 500/50s, so ~50 s remain.
        assert_eq!(
            eta_seconds_from(9500, 10000, Some(9000), Duration::from_secs(50)),
            Some(50),
            "ETA must come from units completed after the first report"
        );
        // The old no-baseline computation would have printed ~3 s: the
        // same numbers against a zero baseline.
        assert_eq!(
            eta_seconds_from(9500, 10000, Some(0), Duration::from_secs(50)),
            Some(3)
        );

        // Cold-cache phase equivalent: first report (100, 10000), then
        // 900 units in 50 s -> ~500 s left.
        assert_eq!(
            eta_seconds_from(1000, 10000, Some(100), Duration::from_secs(50)),
            Some(500)
        );

        // No first report yet: no ETA.
        assert_eq!(
            eta_seconds_from(100, 1000, None, Duration::from_secs(50)),
            None
        );
        // No movement since the first report: no ETA.
        assert_eq!(
            eta_seconds_from(9000, 10000, Some(9000), Duration::from_secs(50)),
            None
        );
        // Too few units since the baseline: no ETA.
        assert_eq!(
            eta_seconds_from(9007, 10000, Some(9000), Duration::from_secs(50)),
            None
        );
        // Too little observed time: no ETA.
        assert_eq!(
            eta_seconds_from(9500, 10000, Some(9000), Duration::from_millis(50)),
            None
        );
        // Sub-second remainder: omitted.
        assert_eq!(
            eta_seconds_from(9999, 10000, Some(9000), Duration::from_secs(50)),
            None
        );
    }

    /// Drives the in-place (TTY) line lifecycle against an in-memory sink:
    /// updates rewrite the line without terminating it, phase finish and
    /// `finish` terminate it exactly once, and append mode never holds an
    /// open line.
    #[test]
    fn in_place_lifecycle_terminates_the_line() {
        let reporter = ProgressReporter::new(false);
        let mut buf: Vec<u8> = Vec::new();

        {
            let mut state = reporter.inner.lock().unwrap();
            state.in_place = true;
            state.phase = Some(IndexPhase::Reconcile);
            state.total = Some(10);
            state.done = 4;
            // Two in-place updates: carriage-return rewrites, no newline.
            state.render(&mut buf);
            state.done = 5;
            state.render(&mut buf);
        }
        let text = String::from_utf8(buf.clone()).unwrap();
        assert!(text.contains('\r'), "in-place renders rewrite the line");
        assert!(!text.contains('\n'), "updates must not terminate the line");

        // Finishing the phase terminates the open line exactly once.
        {
            let mut state = reporter.inner.lock().unwrap();
            state.render(&mut buf);
            state.finish_line(&mut buf);
        }
        let text = String::from_utf8(buf.clone()).unwrap();
        assert!(
            text.ends_with('\n'),
            "the finished phase line is terminated"
        );
        assert_eq!(
            text.matches('\n').count(),
            1,
            "phase finish adds exactly one newline"
        );

        // Termination is idempotent once the line is cleared.
        let before = buf.len();
        reporter.inner.lock().unwrap().finish_line(&mut buf);
        assert_eq!(buf.len(), before, "no extra newline after termination");

        // Append (non-TTY) mode already terminates every line, so
        // finish adds nothing.
        let mut state = reporter.inner.lock().unwrap();
        state.in_place = false;
        state.last_line = String::new();
        state.last_rendered = String::new();
        let before = buf.len();
        state.render(&mut buf);
        state.finish_line(&mut buf);
        let added = String::from_utf8(buf[before..].to_vec()).unwrap();
        assert_eq!(
            added.matches('\n').count(),
            1,
            "append mode contributes only its own line terminator: {added:?}"
        );
    }

    /// A phase's final line is printed exactly once: a repeated completion
    /// report (for example a trailing callback) and the `phase_finished`
    /// that follows it must not repeat a line already on screen.
    #[test]
    fn the_final_line_prints_once() {
        let no_line_repeats = |buf: &[u8]| {
            let text = String::from_utf8(buf.to_vec()).unwrap();
            let mut seen: Vec<&str> = Vec::new();
            assert!(
                !text.lines().any(|line| {
                    let repeated = seen.contains(&line);
                    seen.push(line);
                    repeated
                }),
                "no line may be printed twice: {text}"
            );
        };

        let reporter = ProgressReporter::new(false);

        // Append (non-TTY) mode: completion is reported twice (final batch
        // plus trailing callback), then the phase ends.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut state = reporter.inner.lock().unwrap();
            state.in_place = false;
            state.on_phase_started(&mut buf, IndexPhase::Embedding, Some(4));
            state.on_progress(&mut buf, IndexPhase::Embedding, 2, 4);
            state.on_progress(&mut buf, IndexPhase::Embedding, 4, 4);
            state.on_progress(&mut buf, IndexPhase::Embedding, 4, 4);
            state.on_phase_finished(&mut buf, IndexPhase::Embedding);
        }
        let text = String::from_utf8(buf.clone()).unwrap();
        let completions = text.lines().filter(|line| line.contains("(100%)")).count();
        assert_eq!(completions, 1, "the completion line prints once: {text}");
        no_line_repeats(&buf);

        // A phase that never reports completion still finishes without
        // repeating a line already on screen.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut state = reporter.inner.lock().unwrap();
            state.in_place = false;
            state.on_phase_started(&mut buf, IndexPhase::Publication, None);
            state.on_phase_finished(&mut buf, IndexPhase::Publication);
        }
        no_line_repeats(&buf);

        // In-place (TTY) mode: the completion line is written once and the
        // phase end terminates it exactly once.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut state = reporter.inner.lock().unwrap();
            state.in_place = true;
            state.on_phase_started(&mut buf, IndexPhase::Embedding, Some(4));
            state.on_progress(&mut buf, IndexPhase::Embedding, 4, 4);
            state.on_progress(&mut buf, IndexPhase::Embedding, 4, 4);
            state.on_phase_finished(&mut buf, IndexPhase::Embedding);
        }
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(
            text.matches("(100%)").count(),
            1,
            "the completion line is written once: {text:?}"
        );
        assert_eq!(
            text.matches('\n').count(),
            1,
            "the phase end terminates the line exactly once: {text:?}"
        );
    }
}
