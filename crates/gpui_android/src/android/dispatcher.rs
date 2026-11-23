//! GPUI's executor on Android: main-thread work runs on GPUI's thread (`host::start`), woken through
//! its looper; background work runs on a pool of worker threads; timers on one thread of their own.

use std::collections::BinaryHeap;
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{
    PlatformDispatcher, Priority, PriorityQueueReceiver, PriorityQueueSender, RunnableVariant,
    profiler,
};

const MIN_THREADS: usize = 2;

fn run(runnable: RunnableVariant) {
    let location = runnable.metadata().location;
    let spawned = runnable.metadata().spawned;
    profiler::update_running_task(spawned, location);
    runnable.run();
    profiler::save_task_timing();
}

struct Timer {
    at: Instant,
    seq: u64,
    runnable: RunnableVariant,
}

impl PartialEq for Timer {
    fn eq(&self, other: &Self) -> bool {
        self.at == other.at && self.seq == other.seq
    }
}
impl Eq for Timer {}
impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Timer {
    // The heap is a max-heap: the earliest timer is the greatest.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.at.cmp(&self.at).then(other.seq.cmp(&self.seq))
    }
}

#[derive(Default)]
struct Timers {
    heap: Mutex<(BinaryHeap<Timer>, u64, bool)>,
    changed: Condvar,
}

impl Timers {
    fn add(&self, duration: Duration, runnable: RunnableVariant) {
        let mut state = self.heap.lock().unwrap_or_else(|e| e.into_inner());
        state.1 += 1;
        let seq = state.1;
        state.0.push(Timer { at: Instant::now() + duration, seq, runnable });
        self.changed.notify_one();
    }

    fn run(&self) {
        let mut state = self.heap.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if state.2 {
                // The dispatcher is gone: a timer's task could only be dropped, which panics its
                // awaiter, so the rest are leaked.
                for timer in std::mem::take(&mut state.0).into_vec() {
                    std::mem::forget(timer.runnable);
                }
                return;
            }
            let now = Instant::now();
            match state.0.peek().map(|t| t.at) {
                Some(at) if at <= now => {
                    let timer = state.0.pop().expect("peeked");
                    drop(state);
                    run(timer.runnable);
                    state = self.heap.lock().unwrap_or_else(|e| e.into_inner());
                }
                Some(at) => {
                    state = self.changed.wait_timeout(state, at - now).unwrap_or_else(|e| e.into_inner()).0;
                }
                None => {
                    state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
                }
            }
        }
    }

    fn stop(&self) {
        self.heap.lock().unwrap_or_else(|e| e.into_inner()).2 = true;
        self.changed.notify_one();
    }
}

pub(crate) struct AndroidDispatcher {
    main_sender: PriorityQueueSender<RunnableVariant>,
    background_sender: PriorityQueueSender<RunnableVariant>,
    timers: std::sync::Arc<Timers>,
    _threads: Vec<thread::JoinHandle<()>>,
    main_thread_id: thread::ThreadId,
}

impl AndroidDispatcher {
    /// Made on GPUI's thread, which becomes its main thread. Answers the dispatcher and the main
    /// thread's queue, which the event loop drains after each wake.
    pub fn new() -> (Self, PriorityQueueReceiver<RunnableVariant>) {
        let (main_sender, main_receiver) = PriorityQueueReceiver::new();
        let (background_sender, background_receiver) = PriorityQueueReceiver::new();
        let count = thread::available_parallelism().map_or(MIN_THREADS, |n| n.get().max(MIN_THREADS));
        let mut threads: Vec<_> = (0..count)
            .map(|i| {
                let receiver: PriorityQueueReceiver<RunnableVariant> = background_receiver.clone();
                thread::Builder::new()
                    .name(format!("Worker-{i}"))
                    .spawn(move || {
                        for runnable in receiver.iter() {
                            run(runnable);
                        }
                    })
                    .expect("could not start a worker thread")
            })
            .collect();
        let timers = std::sync::Arc::new(Timers::default());
        let timer_thread = timers.clone();
        threads.push(
            thread::Builder::new()
                .name("Timer".to_owned())
                .spawn(move || timer_thread.run())
                .expect("could not start the timer thread"),
        );
        let dispatcher = Self {
            main_sender,
            background_sender,
            timers,
            _threads: threads,
            main_thread_id: thread::current().id(),
        };
        (dispatcher, main_receiver)
    }
}

impl Drop for AndroidDispatcher {
    fn drop(&mut self) {
        self.timers.stop();
    }
}

impl PlatformDispatcher for AndroidDispatcher {
    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main_thread_id
    }

    fn dispatch(&self, runnable: RunnableVariant, priority: Priority) {
        if let Err(e) = self.background_sender.send(priority, runnable) {
            // The workers are gone: the process is ending.
            std::mem::forget(e);
        }
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, priority: Priority) {
        match self.main_sender.send(priority, runnable) {
            Ok(()) => super::host::wake(),
            // The runnable may hold a future that is !Send, and this may be another thread: it is
            // leaked, as the main loop has ended and the app with it.
            Err(e) => std::mem::forget(e),
        }
    }

    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        self.timers.add(duration, runnable);
    }

    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        thread::spawn(move || {
            // SAFETY: always safe to call; the policy and priority are valid for SCHED_FIFO.
            unsafe {
                let mut param: libc::sched_param = std::mem::zeroed();
                param.sched_priority = 65;
                if libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) != 0 {
                    log::warn!("failed to set realtime thread priority");
                }
            }
            f();
        });
    }
}

/// Runs what the main thread's queue holds now.
pub(crate) fn drain(receiver: &mut PriorityQueueReceiver<RunnableVariant>) {
    while let Ok(Some(runnable)) = receiver.try_pop() {
        run(runnable);
    }
}
