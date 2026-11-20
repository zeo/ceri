// a phase profiler for the engine's own work: named scopes add up their time and calls on the
// thread that runs them while profiling is on, and cost one thread-local read when it is off

use std::cell::{Cell, RefCell};
use std::time::Instant;

#[derive(Clone, Copy, Default)]
struct Phase {
    name: &'static str,
    calls: u64,
    // time inside the scope, and the part of it spent in scopes nested within
    total: u64,
    nested: u64,
}

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static PHASES: RefCell<Vec<Phase>> = const { RefCell::new(Vec::new()) };
    // the nested time of each open scope, innermost last
    static OPEN: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

pub struct Scope {
    name: &'static str,
    started: Option<Instant>,
}

// times the code until the guard drops, under a name shared by every call site that uses it
pub fn scope(name: &'static str) -> Scope {
    let started = ENABLED.with(Cell::get).then(|| {
        OPEN.with(|open| open.borrow_mut().push(0));
        Instant::now()
    });
    Scope { name, started }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let Some(started) = self.started else {
            return;
        };
        let total = started.elapsed().as_nanos() as u64;
        let nested = OPEN.with(|open| {
            let mut open = open.borrow_mut();
            let nested = open.pop().unwrap_or(0);
            if let Some(parent) = open.last_mut() {
                *parent += total;
            }
            nested
        });
        PHASES.with(|phases| {
            let mut phases = phases.borrow_mut();
            let phase = match phases.iter_mut().find(|phase| phase.name == self.name) {
                Some(phase) => phase,
                None => {
                    phases.push(Phase {
                        name: self.name,
                        ..Phase::default()
                    });
                    phases.last_mut().expect("a phase was just pushed")
                }
            };
            phase.calls += 1;
            phase.total += total;
            phase.nested += nested;
        });
    }
}

// starts or stops collecting on this thread, forgetting what was collected before
pub fn enable(on: bool) {
    ENABLED.with(|enabled| enabled.set(on));
    PHASES.with(|phases| phases.borrow_mut().clear());
    OPEN.with(|open| open.borrow_mut().clear());
}

// what each phase has added up since collecting began, the most time of its own first: its
// name, the seconds of its own, the seconds with what it called, and its calls
pub fn phases() -> Vec<(&'static str, f64, f64, u64)> {
    let mut phases = PHASES.with(|phases| phases.borrow().clone());
    phases.sort_by_key(|phase| std::cmp::Reverse(phase.total.saturating_sub(phase.nested)));
    phases
        .into_iter()
        .map(|phase| {
            let own = phase.total.saturating_sub(phase.nested);
            (
                phase.name,
                own as f64 / 1e9,
                phase.total as f64 / 1e9,
                phase.calls,
            )
        })
        .collect()
}

// one line per phase: milliseconds per frame of its own and with what it called, and calls
// per frame
pub fn report(frames: usize) -> String {
    let frames = frames.max(1) as f64;
    let mut lines = vec![format!(
        "{:<28} {:>9} {:>9} {:>9}",
        "phase", "self ms", "total ms", "calls"
    )];
    for (name, own, total, calls) in phases() {
        lines.push(format!(
            "{:<28} {:>9.3} {:>9.3} {:>9.1}",
            name,
            own * 1e3 / frames,
            total * 1e3 / frames,
            calls as f64 / frames
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_count_calls_and_keep_nested_time_out_of_self_time() {
        enable(true);
        {
            let _outer = scope("outer");
            for _ in 0..3 {
                let _inner = scope("inner");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        let phases = PHASES.with(|phases| phases.borrow().clone());
        let find = |name: &str| *phases.iter().find(|phase| phase.name == name).expect(name);
        let (outer, inner) = (find("outer"), find("inner"));
        assert_eq!((outer.calls, inner.calls), (1, 3));
        assert!(inner.total >= 6_000_000);
        assert_eq!(outer.nested, inner.total);
        assert!(outer.total >= outer.nested);
        assert!(report(1).lines().count() == 3);
        enable(false);
        drop(scope("ignored"));
        assert!(PHASES.with(|phases| phases.borrow().is_empty()));
    }
}
