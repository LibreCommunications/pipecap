//! Multi-source audio mixer.
//!
//! Each capture stream pushes its decoded f32 samples into a per-source
//! queue keyed by the PipeWire node id. `drain()` consumes the largest
//! prefix common to all currently-active sources, summing them sample-by-
//! sample. Sources that have not produced data recently are pruned so a
//! stalled stream cannot block the rest forever.
//!
//! Single-source mode (system sink monitor, single-app capture) just uses
//! the same buffer with one synthetic source id — no allocation overhead
//! beyond what a `VecDeque<f32>` already does.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{AudioBuffer, MAX_SAMPLES};

/// Drop a source from the mix if it has not produced any samples in this
/// long. Without this a paused/stalled stream would freeze `drain()` because
/// the per-source min length stays at zero.
const STALE_AFTER: Duration = Duration::from_millis(500);

struct SourceState {
    queue: VecDeque<f32>,
    last_write: Instant,
}

struct MixInner {
    sources: HashMap<u32, SourceState>,
    channels: u32,
    sample_rate: u32,
}

pub struct MixBuffer {
    inner: Mutex<MixInner>,
}

impl MixBuffer {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(MixInner {
                sources: HashMap::new(),
                channels: 2,
                sample_rate: 48000,
            }),
        }
    }

    pub fn set_format(&self, channels: u32, sample_rate: u32) {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        g.channels = channels;
        g.sample_rate = sample_rate;
    }

    /// Push samples from a single source. `node_id` may be a real PipeWire
    /// node id or a synthetic id for the single-source case.
    pub fn push(&self, node_id: u32, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let s = g.sources.entry(node_id).or_insert_with(|| SourceState {
            queue: VecDeque::with_capacity(MAX_SAMPLES / 4),
            last_write: Instant::now(),
        });
        s.queue.extend(samples.iter().copied());
        s.last_write = Instant::now();
        // Per-source bound: drop oldest if a single source runs away
        // (e.g. consumer not draining). Keep MAX_SAMPLES of headroom per
        // source so the mix has slack to align across sources.
        if s.queue.len() > MAX_SAMPLES {
            let drop = s.queue.len() - MAX_SAMPLES;
            // VecDeque::drain(..n) is O(n) but only runs on overflow, and
            // is still better than the previous Vec<f32>::drain shift.
            s.queue.drain(..drop);
        }
    }

    pub fn remove_source(&self, node_id: u32) {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        g.sources.remove(&node_id);
    }

    /// Drain the largest prefix common to all live sources, mixed.
    /// Returns `None` if no source has any samples ready.
    pub fn drain(&self) -> Option<AudioBuffer> {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        // Prune sources that have gone silent so they can't block the mix.
        let now = Instant::now();
        g.sources
            .retain(|_, s| now.duration_since(s.last_write) < STALE_AFTER || !s.queue.is_empty());

        if g.sources.is_empty() {
            return None;
        }
        // Active = has samples; if none of the (non-pruned) sources has
        // samples, nothing to drain yet.
        let active_min = g
            .sources
            .values()
            .filter(|s| !s.queue.is_empty())
            .map(|s| s.queue.len())
            .min()
            .unwrap_or(0);
        if active_min == 0 {
            return None;
        }

        // Mix sample-by-sample. Single-source mode just memcpy-ish since
        // the loop only runs once.
        let mut out: Vec<f32> = vec![0.0; active_min];
        for s in g.sources.values_mut() {
            // Skip sources that exist but currently empty (rare race).
            if s.queue.is_empty() {
                continue;
            }
            // Drain `active_min` samples from this source's front.
            let take = active_min.min(s.queue.len());
            for (i, v) in s.queue.drain(..take).enumerate() {
                out[i] += v;
            }
        }

        Some(AudioBuffer {
            channels: g.channels,
            sample_rate: g.sample_rate,
            data: out,
        })
    }
}
