//! Work the engine runs later, the twin of `$.clock.after` and `$.clock.every`.
//!
//! The engine keeps its own timers: when one fires, its work runs as detached work on the hub, so
//! its host calls go out on stdout like any other engine-initiated work. A timer dies with the
//! engine; a feature that needs one after a respawn sets it again on the start the relay re-posts.

use super::host::{BoxFuture, Host, Hub};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

/// What a timer runs each time it fires.
pub type TimerWork = Arc<dyn Fn(Arc<dyn Host>) -> BoxFuture<()> + Send + Sync>;

pub trait Timers: Send + Sync {
    /// Run `work` once, `ms` from now. The id cancels it.
    fn after(&self, ms: f64, work: TimerWork) -> u64;
    /// Run `work` every `ms`, the first time `ms` from now. A run still going when the next is
    /// due does not hold it back, as with `$.clock.every`.
    fn every(&self, ms: f64, work: TimerWork) -> u64;
    /// Stop a timer; one already fired or cancelled is left alone.
    fn cancel(&self, id: u64);
}

/// Timers on the tokio clock, whose work runs on the hub.
pub struct HubTimers {
    hub: Arc<Hub>,
    next: AtomicU64,
    live: Arc<Mutex<HashSet<u64>>>,
}

impl HubTimers {
    pub fn new(hub: Arc<Hub>) -> Arc<Self> {
        Arc::new(Self {
            hub,
            next: AtomicU64::new(0),
            live: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    fn arm(&self) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        lock(&self.live).insert(id);
        id
    }
}

fn lock(live: &Mutex<HashSet<u64>>) -> std::sync::MutexGuard<'_, HashSet<u64>> {
    live.lock().unwrap_or_else(|e| e.into_inner())
}

/// At least a millisecond: a zero period would spin.
fn period(ms: f64) -> Duration {
    Duration::from_millis(ms.max(1.) as u64)
}

impl Timers for HubTimers {
    fn after(&self, ms: f64, work: TimerWork) -> u64 {
        let id = self.arm();
        let (hub, live) = (self.hub.clone(), self.live.clone());
        tokio::spawn(async move {
            tokio::time::sleep(period(ms)).await;
            if lock(&live).remove(&id) {
                hub.detach(move |host| work(host));
            }
        });
        id
    }

    fn every(&self, ms: f64, work: TimerWork) -> u64 {
        let id = self.arm();
        let (hub, live) = (self.hub.clone(), self.live.clone());
        let each = period(ms);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + each, each);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if !lock(&live).contains(&id) {
                    break;
                }
                let work = work.clone();
                hub.detach(move |host| work(host));
            }
        });
        id
    }

    fn cancel(&self, id: u64) {
        lock(&self.live).remove(&id);
    }
}

/// Timers that fire only when a test moves the clock, the twin of the mods test kit's
/// `mock.clock`.
#[cfg(test)]
pub mod fake {
    use super::super::host::{fake::FakeClock, Clock};
    use super::*;

    struct Row {
        id: u64,
        due: f64,
        every: Option<f64>,
        work: TimerWork,
    }

    pub struct FakeTimers {
        clock: Arc<FakeClock>,
        next: AtomicU64,
        rows: Mutex<Vec<Row>>,
    }

    impl FakeTimers {
        pub fn new(clock: Arc<FakeClock>) -> Arc<Self> {
            Arc::new(Self {
                clock,
                next: AtomicU64::new(0),
                rows: Mutex::new(Vec::new()),
            })
        }

        fn rows(&self) -> std::sync::MutexGuard<'_, Vec<Row>> {
            self.rows.lock().unwrap_or_else(|e| e.into_inner())
        }

        fn set(&self, ms: f64, every: Option<f64>, work: TimerWork) -> u64 {
            let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
            let due = self.clock.now() + ms;
            self.rows().push(Row {
                id,
                due,
                every,
                work,
            });
            id
        }

        /// Move the clock `ms` on, firing each timer due on the way at its own time and in due
        /// order; an every timer fires as often as its period fits. Each run is awaited before
        /// the next, with `host` as its host.
        pub async fn advance(&self, ms: f64, host: Arc<dyn Host>) {
            let until = self.clock.now() + ms;
            loop {
                let due = {
                    let mut rows = self.rows();
                    let soonest = (rows.iter().enumerate())
                        .filter(|(_, row)| row.due <= until)
                        .min_by(|a, b| a.1.due.total_cmp(&b.1.due).then(a.1.id.cmp(&b.1.id)))
                        .map(|(at, _)| at);
                    soonest.map(|at| match rows[at].every {
                        Some(each) => {
                            let row = &mut rows[at];
                            let due = row.due;
                            row.due += each.max(1.);
                            (due, row.work.clone())
                        }
                        None => {
                            let row = rows.remove(at);
                            (row.due, row.work)
                        }
                    })
                };
                let Some((at, work)) = due else { break };
                self.clock.set(at);
                work(host.clone()).await;
            }
            self.clock.set(until);
        }

        /// The periods of the every timers still set, in the order they were set.
        pub fn everies(&self) -> Vec<f64> {
            self.rows().iter().filter_map(|row| row.every).collect()
        }

        /// How many timers are still set.
        pub fn pending(&self) -> usize {
            self.rows().len()
        }

        /// Drop every timer's work. A feature's timer closes over the `Ctx`, and the `Ctx` holds
        /// these timers: left alone the pair keeps each other alive, and with them the store's
        /// open database, for the rest of the test run.
        pub fn clear(&self) {
            let rows = std::mem::take(&mut *self.rows());
            drop(rows);
        }
    }

    impl Timers for FakeTimers {
        fn after(&self, ms: f64, work: TimerWork) -> u64 {
            self.set(ms, None, work)
        }

        fn every(&self, ms: f64, work: TimerWork) -> u64 {
            self.set(ms, Some(ms), work)
        }

        fn cancel(&self, id: u64) {
            self.rows().retain(|row| row.id != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::host::{
        fake::{FakeClock, FakeHost, World},
        Clock,
    };
    use super::fake::FakeTimers;
    use super::*;
    use tokio::sync::mpsc;

    fn count(into: &Arc<Mutex<Vec<f64>>>, clock: &Arc<FakeClock>) -> TimerWork {
        let (into, clock) = (into.clone(), clock.clone());
        Arc::new(move |_host| {
            let (into, clock) = (into.clone(), clock.clone());
            Box::pin(async move { into.lock().unwrap().push(clock.now()) })
        })
    }

    #[tokio::test]
    async fn an_after_fires_once_at_its_time_and_an_every_as_often_as_its_period_fits() {
        let host = FakeHost::new(World::default());
        let clock = host.clock.clone();
        let start = clock.now();
        let timers = FakeTimers::new(clock.clone());
        let (once, each) = (Arc::default(), Arc::default());
        timers.after(1_500., count(&once, &clock));
        timers.every(1_000., count(&each, &clock));
        assert_eq!(timers.everies(), vec![1_000.]);
        timers.advance(999., Arc::new(host.clone())).await;
        assert!(each.lock().unwrap().is_empty());
        timers.advance(2_001., Arc::new(host.clone())).await;
        assert_eq!(*once.lock().unwrap(), vec![start + 1_500.]);
        let at: Vec<f64> = each.lock().unwrap().iter().map(|t| t - start).collect();
        assert_eq!(at, vec![1_000., 2_000., 3_000.]);
        assert_eq!(clock.now(), start + 3_000.);
        assert_eq!(timers.pending(), 1);
    }

    #[tokio::test]
    async fn a_cancelled_timer_never_fires() {
        let host = FakeHost::new(World::default());
        let clock = host.clock.clone();
        let timers = FakeTimers::new(clock.clone());
        let fired = Arc::default();
        let id = timers.every(10., count(&fired, &clock));
        timers.cancel(id);
        timers.advance(100., Arc::new(host)).await;
        assert!(fired.lock().unwrap().is_empty());
        assert_eq!(timers.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_hub_runs_a_due_timer_as_detached_work_and_a_cancelled_one_not_at_all() {
        let (push, mut lines) = mpsc::unbounded_channel();
        let timers = HubTimers::new(Hub::new(push));
        let asks = |text: &'static str| -> TimerWork {
            Arc::new(move |host| {
                Box::pin(async move {
                    let _ = host.call("ui.toast", vec![serde_json::json!(text)]).await;
                })
            })
        };
        timers.after(50., asks("later"));
        let gone = timers.after(50., asks("cancelled"));
        timers.cancel(gone);
        let id = timers.every(100., asks("tick"));
        tokio::time::sleep(Duration::from_millis(250)).await;
        timers.cancel(id);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut said = Vec::new();
        while let Ok(line) = lines.try_recv() {
            let row: serde_json::Value = serde_json::from_str(&line).unwrap();
            said.push(row["calls"][0]["a"][0].as_str().unwrap_or("").to_string());
        }
        assert_eq!(said, vec!["later", "tick", "tick"]);
    }
}
