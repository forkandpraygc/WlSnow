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
use wayland_client::{Connection, QueueHandle};

use crate::control::SharedState;
use crate::render::{render_frame, BPP};
use crate::sim::Simulation;

/// Connect to the compositor, set up a background layer surface, and run the
/// frame loop until the control protocol asks us to quit (or the surface is
/// closed).
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
        layer: None,
        pool: None,
        output: None,
        width: 0,
        height: 0,
        scale: 1,
        sim: None,
        last_frame: Instant::now(),
        running: true,
    };

    // Populate the output info (logical size + scale factor) before sizing the
    // layer surface.
    event_queue.roundtrip(&mut app)?;

    app.setup(&qh, &compositor, &layer_shell)?;

    while app.running {
        event_queue.blocking_dispatch(&mut app)?;
    }

    Ok(())
}

struct App {
    state: Arc<Mutex<SharedState>>,
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,

    layer: Option<LayerSurface>,
    pool: Option<SlotPool>,
    output: Option<wl_output::WlOutput>,

    /// Logical surface size, updated by `configure`.
    width: u32,
    height: u32,
    /// Integer buffer scale factor.
    scale: i32,
    sim: Option<Simulation>,
    last_frame: Instant,
    running: bool,
}

impl App {
    /// Pick the first output, create the background layer surface sized to it,
    /// allocate the shm pool, and perform the initial commit.
    fn setup(
        &mut self,
        qh: &QueueHandle<Self>,
        compositor: &CompositorState,
        layer_shell: &LayerShell,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let output = match self.output_state.outputs().next() {
            Some(output) => output,
            None => {
                eprintln!("wlsnow: no wl_output available");
                return Err("no wl_output available".into());
            }
        };

        let info = self.output_state.info(&output);
        let scale = info.as_ref().map(|i| i.scale_factor.max(1)).unwrap_or(1);
        let (width, height) = info.as_ref().map(logical_size).unwrap_or((800, 600));

        self.width = width;
        self.height = height;
        self.scale = scale;
        self.output = Some(output.clone());

        let surface = compositor.create_surface(qh);
        let layer = layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Background,
            Some("wlsnow"),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(width, height);
        self.layer = Some(layer);

        let pool = SlotPool::new((width as usize) * (height as usize) * BPP, &self.shm)?;
        self.pool = Some(pool);

        // Initial commit (no buffer) triggers the first `configure`.
        self.layer.as_ref().expect("layer surface created").commit();

        Ok(())
    }

    /// Render one frame: advance the simulation and commit a fresh buffer, then
    /// request the next frame callback.
    fn draw(&mut self, qh: &QueueHandle<Self>) {
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

        let scale = self.scale.max(1) as u32;
        let buf_w = self.width.saturating_mul(scale);
        let buf_h = self.height.saturating_mul(scale);
        if buf_w == 0 || buf_h == 0 {
            // Not configured yet; nothing to draw.
            return;
        }

        let stride = buf_w as usize * BPP;

        // Rebuild the simulation if the flake count or the (scaled) output size
        // changed.
        let rebuild = match &self.sim {
            Some(sim) => {
                sim.width != buf_w || sim.height != buf_h || sim.flakes.len() != config.flake_count
            }
            None => true,
        };
        if rebuild {
            let mut rng = rand::rng();
            self.sim = Some(Simulation::new(config.flake_count, buf_w, buf_h, &config, &mut rng));
        }
        let sim = self.sim.as_mut().expect("simulation initialized");

        if !paused {
            sim.step(dt);
        }

        let layer = match &self.layer {
            Some(layer) => layer,
            None => return,
        };
        let pool = match &mut self.pool {
            Some(pool) => pool,
            None => return,
        };

        let (buffer, canvas) = match pool.create_buffer(
            buf_w as i32,
            buf_h as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
        ) {
            Ok(buffer) => buffer,
            Err(e) => {
                eprintln!("wlsnow: failed to allocate buffer: {e}");
                self.running = false;
                return;
            }
        };

        render_frame(canvas, buf_w, buf_h, stride as u32, &sim.flakes);

        let surface = layer.wl_surface();
        surface.damage_buffer(0, 0, buf_w as i32, buf_h as i32);
        surface.set_buffer_scale(scale as i32);
        surface.frame(qh, FrameCallbackData(surface.clone()));
        if let Err(e) = buffer.attach_to(surface) {
            eprintln!("wlsnow: failed to attach buffer: {e}");
            self.running = false;
            return;
        }
        layer.commit();
    }
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
        _surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        self.scale = new_factor.max(1);
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
        self.draw(qh);
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
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        // A size of zero means "client may choose"; keep the current size.
        if configure.new_size.0 > 0 {
            self.width = configure.new_size.0;
        }
        if configure.new_size.1 > 0 {
            self.height = configure.new_size.1;
        }

        if let Some(output) = &self.output {
            if let Some(info) = self.output_state.info(output) {
                self.scale = info.scale_factor.max(1);
            }
        }

        // Draw on the first configure; later frames are driven by frame callbacks.
        if self.sim.is_none() {
            self.draw(qh);
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
