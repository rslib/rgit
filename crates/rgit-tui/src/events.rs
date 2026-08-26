use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossterm::event::{self, Event as CtEvent, KeyEvent, KeyEventKind, MouseEvent};
use tokio::sync::mpsc;
use tokio::task::JoinHandle as TaskHandle;

const POLL: Duration = Duration::from_millis(100);

/// A terminal or timer event. Input and a slow logic tick are unified into one
/// stream the runtime selects on; drawing is driven on demand, not by a pulse.
#[derive(Debug)]
pub enum Event {
    Tick,
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize,
    Error,
}

/// The event source. Input is read on a dedicated thread that polls with a
/// timeout and honors a stop flag, so it can be paused to release stdin to a
/// child process (an external editor) and resumed afterward.
pub struct Events {
    rx: mpsc::UnboundedReceiver<Event>,
    stop: Arc<AtomicBool>,
    input: Option<JoinHandle<()>>,
    timers: TaskHandle<()>,
    tick_hz: f64,
    frame_hz: f64,
}

impl Events {
    pub fn new(tick_hz: f64, frame_hz: f64) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let input = spawn_input(tx.clone(), stop.clone());
        let timers = spawn_timers(tx, tick_hz, frame_hz);
        Self {
            rx,
            stop,
            input: Some(input),
            timers,
            tick_hz,
            frame_hz,
        }
    }

    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }

    /// Stop reading input (freeing stdin for a child process) and pause timers.
    /// Blocks briefly while the input thread finishes its current poll.
    pub fn pause(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.timers.abort();
        if let Some(handle) = self.input.take() {
            let _ = handle.join();
        }
    }

    /// Resume input and timers on a fresh channel after a [`pause`](Self::pause).
    pub fn resume(&mut self) {
        let (tx, rx) = mpsc::unbounded_channel();
        self.rx = rx;
        self.stop = Arc::new(AtomicBool::new(false));
        self.input = Some(spawn_input(tx.clone(), self.stop.clone()));
        self.timers = spawn_timers(tx, self.tick_hz, self.frame_hz);
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.timers.abort();
        // The input thread exits on its own within one poll interval; joining
        // here could block the caller, so leave it to wind down.
    }
}

fn spawn_input(tx: mpsc::UnboundedSender<Event>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let event = match event::poll(POLL) {
                Ok(true) => match event::read() {
                    Ok(CtEvent::Key(key)) if key.kind != KeyEventKind::Release => Event::Key(key),
                    Ok(CtEvent::Mouse(m)) => Event::Mouse(m),
                    Ok(CtEvent::Resize(_, _)) => Event::Resize,
                    Ok(_) => continue,
                    Err(_) => Event::Error,
                },
                Ok(false) => continue,
                Err(_) => Event::Error,
            };
            if tx.send(event).is_err() {
                break;
            }
        }
    })
}

fn spawn_timers(tx: mpsc::UnboundedSender<Event>, tick_hz: f64, _frame_hz: f64) -> TaskHandle<()> {
    // The runtime draws on demand (input, async results, animation ticks), so a
    // fixed render pulse is no longer needed; only the logic tick is emitted.
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / tick_hz));
        loop {
            tick.tick().await;
            if tx.send(Event::Tick).is_err() {
                break;
            }
        }
    })
}
