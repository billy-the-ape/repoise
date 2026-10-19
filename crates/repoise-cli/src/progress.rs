//! Human progress rendering for `repoise index`.
//!
//! Core services emit structured phase/counter events
//! (`repoise_core::progress`); this module owns all terminal rendering.
//! Human mode writes progress to stdout alongside the final summary;
//! `--json` mode keeps stdout as the single final JSON document and sends
//! progress to stderr instead. Rendering is throttled (at most one line per
//! 100 ms within a phase; phase changes and phase ends always render) and
//! never emits per-file noise. On a TTY the current phase line is rewritten
//! in place and terminated when the phase finishes (or via `finish`), so the
//! final summary or an error always starts on a new line; otherwise new
//! lines are appended.

use std::io::{self, IsTerminal, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use repoise_core::progress::{IndexPhase, IndexProgressReporter};

/// Minimum interval between progress lines within one phase.
const UPDATE_INTERVAL: Duration = Duration::from_millis(100);
/// Minimum completed units before a per-unit ETA is shown (below this a few
/// slow units skew the estimate too much to be honest).
const MIN_UNITS_FOR_ETA: u64 = 8;
/// Minimum observed phase time before an ETA is shown.
const MIN_TIME_FOR_ETA: Duration = Duration::from_millis(100);

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
        self.emit(|state, stream| {
            state.phase = Some(phase);
            state.phase_started = Instant::now();
            state.done = 0;
            state.total = total;
            state.last_render = Instant::now();
            state.render(stream);
        });
    }

    fn progress(&self, phase: IndexPhase, done: u64, total: u64) {
        self.emit(|state, stream| {
            state.phase = Some(phase);
            state.done = done;
            state.total = Some(total);
            let now = Instant::now();
            // Completion always renders; otherwise stay within the throttle.
            if done == total || now.duration_since(state.last_render) >= UPDATE_INTERVAL {
                state.last_render = now;
                state.render(stream);
            }
        });
    }

    fn phase_finished(&self, phase: IndexPhase) {
        self.emit(|state, stream| {
            state.phase = Some(phase);
            state.last_render = Instant::now();
            state.render(stream);
            // A finished phase stays visible: terminate its in-place line so
            // the next phase starts on a new line.
            state.finish_line(stream);
        });
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
    fn render(&mut self, stream: &mut dyn Write) {
        let line = self.line();
        if line.is_empty() {
            return;
        }
        if self.in_place {
            let pad = " ".repeat(self.last_line.len().saturating_sub(line.len()));
            let _ = write!(stream, "\r{line}{pad}");
        } else {
            let _ = writeln!(stream, "{line}");
        }
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
    /// misleading (too few units, too little observed time, or negligible).
    fn eta_seconds(&self) -> Option<u64> {
        let total = self.total?;
        if self.done < MIN_UNITS_FOR_ETA || total <= self.done {
            return None;
        }
        let observed = self.phase_started.elapsed();
        if observed < MIN_TIME_FOR_ETA {
            return None;
        }
        let observed_secs = observed.as_secs_f64();
        if observed_secs <= 0.0 {
            return None;
        }
        let remaining_secs = (total - self.done) as f64 / (self.done as f64 / observed_secs);
        if remaining_secs < 1.0 {
            return None;
        }
        Some(remaining_secs.round().max(1.0) as u64)
    }
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
}
