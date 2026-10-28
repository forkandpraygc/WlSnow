use std::sync::{Arc, Mutex};
use std::time::Instant;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, FrameCallbackData};
use smithay_client_toolkit::delegate_registry;
use smithay_client_toolkit::output::{OutputHandler, OutputInfo, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_output, wl_shm, wl_surface};
use wayland_client::{Connection, Proxy, QueueHandle};

use crate::config::Config;
use crate::control::SharedState;
use crate::render::{render_frame, BPP};
use crate::sim::Simulation;

/// Connect to the compositor, set up a `bottom` layer surface per output (above
/// the desktop wallpaper, below windows), and run the frame loop until the
/// control protocol asks us to quit (or a surface is closed).
pub fn run(state: Arc<Mutex<SharedState>>) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|_| "compositor does not support wl_compositor")?;
    let shm = Shm::bind(&globals, &qh).map_err(|_| "compositor does not support wl_shm")?;
    let layer_shell = match LayerShell::bind(&globals, &qh) {
        Ok(ls) => ls,
        Err(e) => {
            eprintln!("wlsnow: compositor does not support wlr-layer-shell ({e})");
            return Err("compositor does not support wlr-layer-shell".into());
        }
    };

    let mut app = App {
        state,
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        surfaces: Vec::new(),
        last_frame: Instant::now(),
        running: true,
        error: None,
    };

    // Populate output info (logical size + scale) before sizing the surfaces.
    event_queue.roundtrip(&mut app)?;

    app.setup(&qh, &compositor, &layer_shell)?;

    // Flush the configure events produced by the initial commits, then start
    // the animation.
    event_queue.roundtrip(&mut app)?;
    app.draw_all(&qh);

    while app.running {
        event_queue.blocking_dispatch(&mut app)?;
    }

    if let Some(err) = app.error {
        return Err(err);
    }

    Ok(())
}

/// One output's layer surface, shm pool, and snow simulation.
struct Surface {
    layer: LayerSurface,
    pool: SlotPool,
    output: wl_output::WlOutput,
    width: u32,
    height: u32,
    scale: i32,
    sim: Option<Simulation>,
    sim_config: Option<Config>,
}

struct App {
    state: Arc<Mutex<SharedState>>,
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    surfaces: Vec<Surface>,
    last_frame: Instant,
    running: bool,
    /// Set when a render-time failure (buffer allocation/attach) ends the loop;
    /// returned by `run()` so the process exits non-zero.
    error: Option<Box<dyn std::error::Error>>,
}

impl App {
    /// Create one background layer surface per output, sized to the output's
    /// logical size, and perform the initial commit on each.
    fn setup(
        &mut self,
        qh: &QueueHandle<Self>,
        compositor: &CompositorState,
        layer_shell: &LayerShell,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let outputs: Vec<wl_output::WlOutput> = self.output_state.outputs().collect();
        if outputs.is_empty() {
            eprintln!("wlsnow: no wl_output available");
            return Err("no wl_output available".into());
        }

        for output in outputs {
            let (scale, width, height) = {
                let info = self.output_state.info(&output);
                let scale = info.as_ref().map(|i| i.scale_factor.max(1)).unwrap_or(1);
                let (w, h) = info.as_ref().map(logical_size).unwrap_or((800, 600));
                (scale, w, h)
            };

            let wl_surface = compositor.create_surface(qh);
            let layer = layer_shell.create_layer_surface(
                qh,
                wl_surface,
                Layer::Bottom,
                Some("wlsnow"),
                Some(&output),
            );
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_size(width, height);

            let pool = SlotPool::new((width as usize) * (height as usize) * BPP, &self.shm)?;

            self.surfaces.push(Surface {
                layer,
                pool,
                output,
                width,
                height,
                scale,
                sim: None,
                sim_config: None,
            });
        }

        // Initial commits (no buffer) trigger the first configure per surface.
        for surface in &self.surfaces {
            surface.layer.commit();
        }

        Ok(())
    }

    /// Render and commit every output's surface once, then let the driver
    /// surface's frame callback pace the next frame.
    fn draw_all(&mut self, qh: &QueueHandle<Self>) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;

        let (quit, paused, config) = {
            let st = self.state.lock().expect("shared state poisoned");
            (st.quit, st.paused, st.config.clone())
        };

        if quit {
            self.running = false;
            return;
        }

        let count = self.surfaces.len();
        for idx in 0..count {
            if let Err(e) = draw_surface(&mut self.surfaces[idx], qh, dt, paused, &config, idx == 0)
            {
                eprintln!("wlsnow: {e}");
                self.running = false;
                self.error = Some(e);
                return;
            }
        }
    }
}

/// Render and commit a single output's surface. `request_frame` is true only
/// for the driver surface (index 0), whose frame callback paces the animation.
fn draw_surface(
    surface: &mut Surface,
    qh: &QueueHandle<App>,
    dt: f32,
    paused: bool,
    config: &Config,
    request_frame: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let scale = surface.scale.max(1) as u32;
    let buf_w = surface.width.saturating_mul(scale);
    let buf_h = surface.height.saturating_mul(scale);
    if buf_w == 0 || buf_h == 0 {
        // Not configured yet; nothing to draw.
        return Ok(());
    }
    let stride = buf_w as usize * BPP;

    // Rebuild the simulation if any flake-affecting config field (count, speed,
    // size, drift) changed, or if the (scaled) output size changed.
    let rebuild = match (&surface.sim, &surface.sim_config) {
        (Some(sim), Some(snapshot)) => {
            sim.width != buf_w || sim.height != buf_h || *snapshot != *config
        }
        _ => true,
    };
    if rebuild {
        let mut rng = rand::rng();
        surface.sim = Some(Simulation::new(
            config.flake_count,
            buf_w,
            buf_h,
            config,
            &mut rng,
        ));
        surface.sim_config = Some(config.clone());
    }
    let sim = surface.sim.as_mut().expect("simulation initialized");

    if !paused {
        sim.step(dt);
    }

    let (buffer, canvas) = surface.pool.create_buffer(
        buf_w as i32,
        buf_h as i32,
        stride as i32,
        wl_shm::Format::Argb8888,
    )?;

    render_frame(canvas, buf_w, buf_h, stride as u32, &sim.flakes);

    let wl_surface = surface.layer.wl_surface();
    wl_surface.damage_buffer(0, 0, buf_w as i32, buf_h as i32);
    wl_surface.set_buffer_scale(scale as i32);
    if request_frame {
        wl_surface.frame(qh, FrameCallbackData(wl_surface.clone()));
    }
    buffer.attach_to(wl_surface)?;
    surface.layer.commit();

    Ok(())
}

/// Best-effort logical size of an output, falling back to the current/preferred
/// mode divided by the scale factor.
fn logical_size(info: &OutputInfo) -> (u32, u32) {
    if let Some((w, h)) = info.logical_size {
        return (w.max(0) as u32, h.max(0) as u32);
    }
    let scale = info.scale_factor.max(1) as u32;
    if let Some(mode) = info.modes.iter().find(|m| m.current || m.preferred) {
        return (
            mode.dimensions.0.max(0) as u32 / scale,
            mode.dimensions.1.max(0) as u32 / scale,
        );
    }
    (800, 600)
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if let Some(idx) = self
            .surfaces
            .iter()
            .position(|s| s.layer.wl_surface().id() == surface.id())
        {
            self.surfaces[idx].scale = new_factor.max(1);
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.draw_all(qh);
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.running = false;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        // NOTE: sctk 0.21 already calls `ack_configure(serial)` for us in its
        // dispatch (shell/wlr_layer/dispatch.rs), so we must NOT ack again
        // here — doing so sends the same serial twice and the compositor
        // rejects it with an "invalid configure serial" protocol error.

        let Some(idx) = self
            .surfaces
            .iter()
            .position(|s| s.layer.wl_surface().id() == layer.wl_surface().id())
        else {
            return;
        };

        let output = self.surfaces[idx].output.clone();
        let mut resized = false;
        {
            let s = &mut self.surfaces[idx];
            // A size of zero means "client may choose"; keep the current size.
            if configure.new_size.0 > 0 && configure.new_size.0 != s.width {
                s.width = configure.new_size.0;
                resized = true;
            }
            if configure.new_size.1 > 0 && configure.new_size.1 != s.height {
                s.height = configure.new_size.1;
                resized = true;
            }
        }
        if let Some(info) = self.output_state.info(&output) {
            let scale = info.scale_factor.max(1);
            if scale != self.surfaces[idx].scale {
                self.surfaces[idx].scale = scale;
                resized = true;
            }
        }

        // Re-commit on a size/scale change after the initial kickoff.
        let started = self.surfaces.iter().any(|s| s.sim.is_some());
        if resized && started {
            self.draw_all(qh);
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}

smithay_client_toolkit::delegate_dispatch2!(App);
