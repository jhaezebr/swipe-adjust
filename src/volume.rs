use crate::config::VolumeConfig;
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet as ContextFlags, State as ContextState};
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::volume::{ChannelVolumes, Volume};
use std::cell::RefCell;
use std::rc::Rc;

const DEFAULT_SINK: &str = "@DEFAULT_SINK@";

pub struct VolumeBackend {
    cfg: VolumeConfig,
    pulse: Option<PulseClient>,
    cached_cvol: ChannelVolumes,
    cached_muted: bool,
    last_raw_vol: Option<u32>,
}

struct PulseClient {
    // `context` MUST be declared (and therefore dropped) before `mainloop`;
    // dropping `pa_mainloop` while `pa_context` still holds time events triggers
    // `Assertion '!e->dead' failed` inside libpulse's `mainloop_time_free()`.
    context: Context,
    mainloop: Mainloop,
}

impl Drop for PulseClient {
    fn drop(&mut self) {
        self.context.disconnect();
    }
}

impl VolumeBackend {
    pub fn new(cfg: &VolumeConfig) -> Self {
        let mut default_cvol = ChannelVolumes::default();
        default_cvol.set(2, Volume::NORMAL);

        let pulse = match PulseClient::connect() {
            Ok(p) => {
                eprintln!("swipe-adjust: audio backend connected to PipeWire/PulseAudio");
                Some(p)
            }
            Err(e) => {
                eprintln!("swipe-adjust: warning: initial PulseAudio connection failed ({e}); will retry on swipe");
                None
            }
        };

        Self {
            cfg: cfg.clone(),
            pulse,
            cached_cvol: default_cvol,
            cached_muted: false,
            last_raw_vol: None,
        }
    }

    pub fn max_norm(&self) -> f64 {
        (self.cfg.max_percent / 100.0).clamp(0.1, 1.5)
    }

    pub fn is_muted(&self) -> bool {
        self.cached_muted
    }

    fn ensure_connected(&mut self) -> bool {
        let healthy = self
            .pulse
            .as_ref()
            .is_some_and(|p| p.context.get_state() == ContextState::Ready);
        if healthy {
            return true;
        }
        self.pulse = PulseClient::connect().ok();
        self.pulse.is_some()
    }

    /// Queries `@DEFAULT_SINK@` synchronously (< 0.5 ms on local Unix socket) when a swipe starts.
    pub fn read_norm(&mut self) -> f64 {
        self.last_raw_vol = None;
        if !self.ensure_connected() {
            return 0.5;
        }

        let result: Rc<RefCell<Option<(ChannelVolumes, bool)>>> = Rc::new(RefCell::new(None));
        let done = Rc::new(RefCell::new(false));

        if let Some(pulse) = self.pulse.as_mut() {
            pulse.drain_nonblocking();

            let res_cb = Rc::clone(&result);
            let done_cb = Rc::clone(&done);
            let _op =
                pulse
                    .context
                    .introspect()
                    .get_sink_info_by_name(DEFAULT_SINK, move |list_res| match list_res {
                        ListResult::Item(info) => {
                            *res_cb.borrow_mut() = Some((info.volume, info.mute));
                        }
                        ListResult::End | ListResult::Error => {
                            *done_cb.borrow_mut() = true;
                        }
                    });

            let mut guard_iters = 0;
            while !*done.borrow() && guard_iters < 64 {
                guard_iters += 1;
                if pulse.context.get_state() != ContextState::Ready {
                    break;
                }
                if matches!(
                    pulse.mainloop.iterate(true),
                    IterateResult::Err(_) | IterateResult::Quit(_)
                ) {
                    break;
                }
            }
        }

        if let Some((cvol, muted)) = result.borrow_mut().take() {
            self.cached_cvol = cvol;
            self.cached_muted = muted;
        }

        if self.cached_muted {
            0.0
        } else {
            let avg = self.cached_cvol.avg().0 as f64;
            (avg / (Volume::NORMAL.0 as f64)).clamp(0.0, self.max_norm())
        }
    }

    /// Updates `@DEFAULT_SINK@` volume (and auto-mute/unmute state) non-blockingly.
    /// Returns `true` if a volume or mute update was dispatched.
    pub fn set_norm(&mut self, norm: f64, swiping_up: bool) -> bool {
        if !self.ensure_connected() {
            return false;
        }

        let clamped = norm.clamp(0.0, self.max_norm());
        let target_raw = ((clamped * (Volume::NORMAL.0 as f64)).round() as u32)
            .clamp(Volume::MUTED.0, (Volume::NORMAL.0 as f64 * 1.5) as u32);

        let mut target_mute = self.cached_muted;
        if clamped <= 0.0005 && self.cfg.auto_mute_at_zero {
            target_mute = true;
        } else if self.cached_muted && swiping_up && clamped > 0.0005 && self.cfg.auto_unmute_on_up
        {
            target_mute = false;
        }

        let vol_changed = self.last_raw_vol != Some(target_raw);
        let mute_changed = target_mute != self.cached_muted;
        if !vol_changed && !mute_changed {
            return false;
        }

        let channels = self.cached_cvol.len().max(1);
        self.cached_cvol.set(channels, Volume(target_raw));
        self.cached_muted = target_mute;
        self.last_raw_vol = Some(target_raw);

        if let Some(pulse) = self.pulse.as_mut() {
            pulse.drain_nonblocking();
            let mut introspector = pulse.context.introspect();
            if vol_changed {
                introspector.set_sink_volume_by_name(DEFAULT_SINK, &self.cached_cvol, None);
            }
            if mute_changed {
                introspector.set_sink_mute_by_name(DEFAULT_SINK, target_mute, None);
            }
            pulse.drain_nonblocking();
            return true;
        }

        false
    }
}

impl PulseClient {
    fn connect() -> Result<Self, String> {
        let mut mainloop =
            Mainloop::new().ok_or_else(|| "failed to create PulseAudio mainloop".to_string())?;
        let mut context = Context::new(&mainloop, "swipe-adjust")
            .ok_or_else(|| "failed to create PulseAudio context".to_string())?;

        context
            .connect(None, ContextFlags::NOFAIL, None)
            .map_err(|e| format!("PulseAudio connect error: {e}"))?;

        loop {
            match mainloop.iterate(true) {
                IterateResult::Success(_) => {}
                IterateResult::Quit(_) | IterateResult::Err(_) => {
                    context.disconnect();
                    return Err("PulseAudio mainloop terminated while connecting".to_string());
                }
            }
            match context.get_state() {
                ContextState::Ready => break,
                ContextState::Failed | ContextState::Terminated => {
                    context.disconnect();
                    return Err("PulseAudio context failed during handshake".to_string());
                }
                _ => {}
            }
        }

        Ok(Self { context, mainloop })
    }

    fn drain_nonblocking(&mut self) {
        for _ in 0..32 {
            match self.mainloop.iterate(false) {
                IterateResult::Success(0) | IterateResult::Quit(_) | IterateResult::Err(_) => break,
                IterateResult::Success(_) => {}
            }
        }
    }
}
