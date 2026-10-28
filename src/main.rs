mod config;
mod control;
mod render;
mod sim;
mod wayland;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = <config::Cli as clap::Parser>::parse();
    let cfg = config::Config::from_cli(&cli);
    let state = std::sync::Arc::new(std::sync::Mutex::new(control::SharedState {
        config: cfg,
        paused: false,
        quit: false,
    }));
    let path = control::socket_path();
    control::spawn_server(path, state.clone());
    wayland::run(state)
}
