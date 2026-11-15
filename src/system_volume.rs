//! Lilac's volume is the volume of its audio stream in the sound server.
//!
//! Desktop shells and mixers show one volume per app. Instead of keeping a
//! second volume of our own on top, the slider sets the stream's volume and
//! follows it when something else changes it, through the PulseAudio
//! protocol that PipeWire also speaks. Changes are pushed by the server, so
//! nothing polls. Without a sound server [`available`] is false and the
//! player applies volume itself.

use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::subscribe::{Facility, InterestMaskSet, Operation};
use pulse::context::{Context, FlagSet, State};
use pulse::mainloop::threaded::Mainloop;
use pulse::proplist::{Proplist, properties};
use pulse::volume::{ChannelVolumes, Volume};

enum Msg {
    /// Set our stream's volume (0..=1 of the server's nominal volume).
    Set(f32),
    /// A stream was added, changed or removed.
    Event { index: u32, removed: bool },
    /// What the server says about one stream.
    Info {
        index: u32,
        ours: bool,
        volume: f32,
        channels: u8,
    },
}

struct SystemVolume {
    tx: Sender<Msg>,
}

static SYSTEM: OnceLock<SystemVolume> = OnceLock::new();
/// How long after our own change stale reports may still arrive.
const SETTLE: Duration = Duration::from_millis(800);
static READY: AtomicBool = AtomicBool::new(false);

/// Connected to a sound server that owns the volume.
pub fn available() -> bool {
    READY.load(Ordering::Acquire)
}

/// Set Lilac's stream volume; applied as soon as the stream exists.
pub fn set(volume: f32) {
    if let Some(system) = SYSTEM.get() {
        let _ = system.tx.send(Msg::Set(volume.clamp(0.0, 1.0)));
    }
}

/// Connect in the background. `on_external` hears every volume change that
/// did not come from [`set`] (the shell's mixer, pavucontrol, …).
pub fn start(on_external: impl Fn(f32) + Send + 'static) {
    let (tx, rx) = channel::<Msg>();
    if SYSTEM.set(SystemVolume { tx: tx.clone() }).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("lilac-volume".into())
        .spawn(move || {
            let Some(server) = Server::connect(tx) else {
                return;
            };
            READY.store(true, Ordering::Release);
            server.run(rx, on_external);
        });
    if spawned.is_err() {
        READY.store(false, Ordering::Release);
    }
}

struct Server {
    mainloop: Rc<RefCell<Mainloop>>,
    context: Rc<RefCell<Context>>,
    tx: Sender<Msg>,
}

impl Server {
    fn connect(tx: Sender<Msg>) -> Option<Self> {
        let mut props = Proplist::new()?;
        props.set_str(properties::APPLICATION_NAME, "Lilac").ok()?;
        props
            .set_str(properties::APPLICATION_ICON_NAME, "lilac")
            .ok()?;
        let mainloop = Rc::new(RefCell::new(Mainloop::new()?));
        let context = Rc::new(RefCell::new(Context::new_with_proplist(
            mainloop.borrow().deref(),
            "Lilac",
            &props,
        )?));

        // The state callback runs on the mainloop thread with its lock held,
        // the only time it touches these.
        {
            let ml = Rc::clone(&mainloop);
            let ctx = Rc::clone(&context);
            context
                .borrow_mut()
                .set_state_callback(Some(Box::new(move || {
                    let state = unsafe { (*ctx.as_ptr()).get_state() };
                    if matches!(state, State::Ready | State::Failed | State::Terminated) {
                        unsafe { (*ml.as_ptr()).signal(false) };
                    }
                })));
        }
        context
            .borrow_mut()
            .connect(None, FlagSet::NOAUTOSPAWN, None)
            .ok()?;
        mainloop.borrow_mut().lock();
        if mainloop.borrow_mut().start().is_err() {
            mainloop.borrow_mut().unlock();
            return None;
        }
        loop {
            match context.borrow().get_state() {
                State::Ready => break,
                State::Failed | State::Terminated => {
                    mainloop.borrow_mut().unlock();
                    return None;
                }
                _ => mainloop.borrow_mut().wait(),
            }
        }
        context.borrow_mut().set_state_callback(None);

        let events = tx.clone();
        context.borrow_mut().set_subscribe_callback(Some(Box::new(
            move |facility, operation, index| {
                if facility == Some(Facility::SinkInput) {
                    let _ = events.send(Msg::Event {
                        index,
                        removed: operation == Some(Operation::Removed),
                    });
                }
            },
        )));
        context
            .borrow_mut()
            .subscribe(InterestMaskSet::SINK_INPUT, |_| {});
        let server = Self {
            mainloop,
            context,
            tx,
        };
        // The stream may already exist (the player started first).
        let tx = server.tx.clone();
        server
            .context
            .borrow()
            .introspect()
            .get_sink_input_info_list(move |item| send_info(&tx, item));
        server.mainloop.borrow_mut().unlock();
        Some(server)
    }

    fn run(self, rx: std::sync::mpsc::Receiver<Msg>, on_external: impl Fn(f32)) {
        let mut ours: Option<(u32, u8)> = None;
        // What Lilac asked for last; reports matching it are our own echo.
        let mut wanted: Option<f32> = None;
        // Until then, reports that do not match `wanted` are stale ones
        // queued before our change reached the server.
        let mut settling_until = Instant::now();
        while let Ok(msg) = rx.recv() {
            match msg {
                Msg::Set(volume) => {
                    wanted = Some(volume);
                    if let Some((index, channels)) = ours {
                        self.apply(index, channels, volume);
                        settling_until = Instant::now() + SETTLE;
                    }
                }
                Msg::Event { index, removed } => {
                    if removed {
                        if ours.is_some_and(|(i, _)| i == index) {
                            ours = None;
                        }
                        continue;
                    }
                    let tx = self.tx.clone();
                    self.mainloop.borrow_mut().lock();
                    self.context
                        .borrow()
                        .introspect()
                        .get_sink_input_info(index, move |item| send_info(&tx, item));
                    self.mainloop.borrow_mut().unlock();
                }
                Msg::Info {
                    index,
                    ours: true,
                    volume,
                    channels,
                } => {
                    let new_stream = ours.is_none_or(|(i, _)| i != index);
                    ours = Some((index, channels));
                    match wanted {
                        Some(w) if (w - volume).abs() <= 0.005 => {}
                        // A new stream starts at whatever the server restored;
                        // Lilac's own setting wins.
                        Some(w) if new_stream => {
                            self.apply(index, channels, w);
                            settling_until = Instant::now() + SETTLE;
                        }
                        Some(_) if Instant::now() < settling_until => {}
                        _ => {
                            wanted = Some(volume);
                            on_external(volume);
                        }
                    }
                }
                Msg::Info { .. } => {}
            }
        }
    }

    fn apply(&self, index: u32, channels: u8, volume: f32) {
        let mut levels = ChannelVolumes::default();
        levels.set(
            channels.max(1),
            Volume((Volume::NORMAL.0 as f32 * volume).round() as u32),
        );
        self.mainloop.borrow_mut().lock();
        self.context
            .borrow()
            .introspect()
            .set_sink_input_volume(index, &levels, None);
        self.mainloop.borrow_mut().unlock();
    }
}

fn send_info(tx: &Sender<Msg>, item: ListResult<&pulse::context::introspect::SinkInputInfo>) {
    let ListResult::Item(info) = item else { return };
    let ours = info
        .proplist
        .get_str(properties::APPLICATION_PROCESS_ID)
        .and_then(|pid| pid.parse::<u32>().ok())
        == Some(std::process::id());
    let _ = tx.send(Msg::Info {
        index: info.index,
        ours,
        volume: info.volume.avg().0 as f32 / Volume::NORMAL.0 as f32,
        channels: info.volume.len(),
    });
}
