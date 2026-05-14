//! Per-backend circuit breaker — prevents cascading latency by removing
//! failing backends from routing proactively.
//!
//! State machine: Closed → Open → HalfOpen → Closed (on success) or Open (on failure).

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};

/// Circuit breaker states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CbState {
    /// Normal operation — traffic flows, errors are counted.
    Closed = 0,
    /// Backend is skipped — no traffic sent until recovery_ms elapses.
    Open = 2,
    /// Probing — one request allowed through to test recovery.
    HalfOpen = 1,
}

impl CbState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Closed,
            1 => Self::HalfOpen,
            2 => Self::Open,
            _ => Self::Closed,
        }
    }
}

/// Per-backend circuit breaker.
///
/// Thread-safe (all fields are atomics). Designed to be stored in a `Vec` alongside
/// `BackendHealth` in `BackendPool`.
pub struct CircuitBreaker {
    /// Current state (0=Closed, 1=HalfOpen, 2=Open).
    state: AtomicU8,
    /// Consecutive errors in Closed state.
    consecutive_errors: AtomicU32,
    /// Epoch seconds when the breaker transitioned to Open.
    opened_at: AtomicU64,
    /// Threshold: consecutive errors to transition Closed → Open.
    threshold: u32,
    /// Time in ms to stay in Open before transitioning to HalfOpen.
    recovery_ms: u64,
}

impl CircuitBreaker {
    pub fn new(threshold: u32, recovery_ms: u64) -> Self {
        Self {
            state: AtomicU8::new(CbState::Closed as u8),
            consecutive_errors: AtomicU32::new(0),
            opened_at: AtomicU64::new(0),
            threshold,
            recovery_ms,
        }
    }

    /// Current state of the circuit breaker.
    pub fn state(&self) -> CbState {
        CbState::from_u8(self.state.load(Ordering::Relaxed))
    }

    /// Returns `true` if traffic should be allowed through this backend.
    ///
    /// - Closed: always allows.
    /// - HalfOpen: allows (one probe).
    /// - Open: blocks unless recovery_ms has elapsed, in which case transitions to HalfOpen.
    pub fn allows(&self) -> bool {
        match self.state() {
            CbState::Closed => true,
            CbState::HalfOpen => true,
            CbState::Open => {
                // Check if recovery time has elapsed.
                let opened = self.opened_at.load(Ordering::Relaxed);
                let now_ms = current_time_ms();
                if now_ms.saturating_sub(opened) >= self.recovery_ms {
                    // Transition to HalfOpen — allow one probe.
                    self.state.store(CbState::HalfOpen as u8, Ordering::Relaxed);
                    log::info!("[CB] backend transitioning Open → HalfOpen (recovery probe)");
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a successful request. Resets error count and closes the breaker.
    pub fn record_success(&self) {
        let prev = self.state();
        self.consecutive_errors.store(0, Ordering::Relaxed);
        if prev != CbState::Closed {
            self.state.store(CbState::Closed as u8, Ordering::Relaxed);
            log::info!("[CB] backend recovered — {} → Closed", state_name(prev));
        }
    }

    /// Record a failed request. May transition Closed → Open or HalfOpen → Open.
    pub fn record_failure(&self) {
        match self.state() {
            CbState::Closed => {
                let errors = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
                if errors >= self.threshold {
                    self.open();
                }
            }
            CbState::HalfOpen => {
                // Probe failed — back to Open.
                self.open();
                log::warn!("[CB] probe failed — HalfOpen → Open");
            }
            CbState::Open => {
                // Already open — nothing to do.
            }
        }
    }

    /// Consecutive error count (for observability).
    #[allow(dead_code)]
    pub fn error_count(&self) -> u32 {
        self.consecutive_errors.load(Ordering::Relaxed)
    }

    fn open(&self) {
        self.state.store(CbState::Open as u8, Ordering::Relaxed);
        self.opened_at.store(current_time_ms(), Ordering::Relaxed);
        self.consecutive_errors.store(0, Ordering::Relaxed);
        log::warn!(
            "[CB] backend circuit OPEN — skipping for {}ms",
            self.recovery_ms
        );
    }
}

fn state_name(s: CbState) -> &'static str {
    match s {
        CbState::Closed => "Closed",
        CbState::HalfOpen => "HalfOpen",
        CbState::Open => "Open",
    }
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
