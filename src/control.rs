use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::config::Config;

/// Version of the control protocol. Requests with a different `v` are rejected.
pub const PROTOCOL_VERSION: u32 = 1;

/// Shared, mutex-guarded state mutated by the control protocol.
pub struct SharedState {
    pub config: Config,
    pub paused: bool,
    pub quit: bool,
}

/// A single control request. `#[serde(default)]` makes every field optional on
/// the wire; missing fields fall back to the derived `Default` (`v = 0`,
/// `cmd = ""`, and `None` for the config overrides).
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Request {
    v: u32,
    cmd: String,
    flake_count: Option<usize>,
    speed: Option<f32>,
    size: Option<f32>,
}

fn error_response(msg: &str) -> String {
    serde_json::json!({ "v": PROTOCOL_VERSION, "ok": false, "error": msg }).to_string()
}

/// Parse one newline-framed request, mutate `state`, and return a JSON response
/// line. Always returns a valid JSON object, even for malformed input.
pub fn handle_line(line: &str, state: &mut SharedState) -> String {
    let req: Request = match serde_json::from_str(line) {
        Ok(req) => req,
        Err(e) => return error_response(&format!("invalid request: {e}")),
    };

    if req.v != PROTOCOL_VERSION {
        return error_response(&format!("unsupported protocol version {}", req.v));
    }

    match req.cmd.as_str() {
        "hello" => serde_json::json!({
            "v": PROTOCOL_VERSION,
            "ok": true,
            "protocol": PROTOCOL_VERSION,
        })
        .to_string(),
        "get_config" => serde_json::json!({
            "v": PROTOCOL_VERSION,
            "ok": true,
            "flake_count": state.config.flake_count,
            "speed": state.config.speed,
            "size": state.config.size,
            "drift": state.config.drift,
            "paused": state.paused,
        })
        .to_string(),
        "set_config" => {
            // Reject non-finite or negative values before mutating anything so a
            // client sending `NaN`/`Infinity` (or an exponent that overflows f32)
            // can't poison the config and force a simulation rebuild every frame.
            if let Some(speed) = req.speed {
                if !speed.is_finite() || speed < 0.0 {
                    return error_response("speed must be a finite, non-negative number");
                }
            }
            if let Some(size) = req.size {
                if !size.is_finite() || size < 0.0 {
                    return error_response("size must be a finite, non-negative number");
                }
            }
            if let Some(count) = req.flake_count {
                state.config.flake_count = count;
            }
            if let Some(speed) = req.speed {
                state.config.speed = speed;
            }
            if let Some(size) = req.size {
                state.config.size = size;
            }
            serde_json::json!({ "v": PROTOCOL_VERSION, "ok": true }).to_string()
        }
        "pause" => {
            state.paused = true;
            serde_json::json!({ "v": PROTOCOL_VERSION, "ok": true }).to_string()
        }
        "resume" => {
            state.paused = false;
            serde_json::json!({ "v": PROTOCOL_VERSION, "ok": true }).to_string()
        }
        "quit" => {
            state.quit = true;
            serde_json::json!({ "v": PROTOCOL_VERSION, "ok": true }).to_string()
        }
        other => error_response(&format!("unknown command: {other}")),
    }
}

/// Location of the control socket. Prefers `$XDG_RUNTIME_DIR/wlsnow.sock` (always
/// present on a Wayland session); otherwise falls back to a per-process path in
/// `/tmp`.
pub fn socket_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("wlsnow.sock");
    }
    PathBuf::from(format!("/tmp/wlsnow-{}.sock", std::process::id()))
}

/// Removes the socket file when the listener is torn down.
struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Bind a `UnixListener` on `path` and serve line-delimited requests on a
/// detached thread. Each connection is handled on its own thread; every line is
/// passed to `handle_line` and the response line is written back. The socket
/// file is removed when the listener drops.
pub fn spawn_server(path: PathBuf, state: Arc<Mutex<SharedState>>) {
    std::thread::spawn(move || {
        // Best-effort remove any stale socket file left behind by a previous
        // run whose detached server thread was killed before its `SocketGuard`
        // could drop. Without this, `bind` would fail with EADDRINUSE and the
        // control channel would be silently lost on relaunch.
        let _ = std::fs::remove_file(&path);

        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(e) => {
                eprintln!(
                    "wlsnow: failed to bind control socket {}: {e}",
                    path.display()
                );
                return;
            }
        };
        let _guard = SocketGuard { path };

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let state = Arc::clone(&state);
                    std::thread::spawn(move || serve_client(stream, state));
                }
                Err(e) => {
                    eprintln!("wlsnow: control socket accept error: {e}");
                }
            }
        }
    });
}

fn serve_client(stream: UnixStream, state: Arc<Mutex<SharedState>>) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let request = line.trim();
                if request.is_empty() {
                    continue;
                }
                let response = {
                    let mut st = state.lock().expect("shared state poisoned");
                    handle_line(request, &mut st)
                };
                let stream = reader.get_mut();
                if stream.write_all(response.as_bytes()).is_err()
                    || stream.write_all(b"\n").is_err()
                    || stream.flush().is_err()
                {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SharedState {
        SharedState {
            config: Config::default(),
            paused: false,
            quit: false,
        }
    }

    #[test]
    fn hello_handshake() {
        let r: serde_json::Value =
            serde_json::from_str(&handle_line(r#"{"v":1,"cmd":"hello"}"#, &mut state())).unwrap();
        assert_eq!(r["v"], 1);
        assert_eq!(r["ok"], true);
        assert_eq!(r["protocol"], 1);
    }

    #[test]
    fn get_config_returns_current() {
        let r: serde_json::Value =
            serde_json::from_str(&handle_line(r#"{"v":1,"cmd":"get_config"}"#, &mut state()))
                .unwrap();
        assert_eq!(r["flake_count"], 200);
        assert_eq!(r["speed"], 60.0);
        assert_eq!(r["paused"], false);
    }

    #[test]
    fn set_config_updates_state() {
        let mut s = state();
        let r: serde_json::Value = serde_json::from_str(&handle_line(
            r#"{"v":1,"cmd":"set_config","flake_count":150,"speed":80.0}"#,
            &mut s,
        ))
        .unwrap();
        assert_eq!(r["ok"], true);
        assert_eq!(s.config.flake_count, 150);
        assert_eq!(s.config.speed, 80.0);
        assert_eq!(s.config.size, 3.0); // untouched
    }

    #[test]
    fn pause_and_quit_set_flags() {
        let mut s = state();
        handle_line(r#"{"v":1,"cmd":"pause"}"#, &mut s);
        assert!(s.paused);
        handle_line(r#"{"v":1,"cmd":"quit"}"#, &mut s);
        assert!(s.quit);
    }

    #[test]
    fn wrong_version_errors() {
        let r: serde_json::Value =
            serde_json::from_str(&handle_line(r#"{"v":99,"cmd":"hello"}"#, &mut state())).unwrap();
        assert_eq!(r["ok"], false);
        assert!(r["error"].is_string());
    }

    #[test]
    fn malformed_line_errors_not_panics() {
        let r: serde_json::Value =
            serde_json::from_str(&handle_line("not json at all", &mut state())).unwrap();
        assert_eq!(r["ok"], false);
    }

    #[test]
    fn unknown_command_errors() {
        let r: serde_json::Value =
            serde_json::from_str(&handle_line(r#"{"v":1,"cmd":"bogus"}"#, &mut state())).unwrap();
        assert_eq!(r["ok"], false);
        assert!(r["error"].is_string());
    }

    #[test]
    fn resume_clears_pause_flag() {
        let mut s = state();
        s.paused = true;
        handle_line(r#"{"v":1,"cmd":"resume"}"#, &mut s);
        assert!(!s.paused);
    }

    #[test]
    fn set_config_rejects_nonfinite_and_negative() {
        let mut s = state();

        let r: serde_json::Value = serde_json::from_str(&handle_line(
            r#"{"v":1,"cmd":"set_config","speed":-1.0}"#,
            &mut s,
        ))
        .unwrap();
        assert_eq!(r["ok"], false);
        assert!(r["error"].is_string());
        assert_eq!(s.config.speed, 60.0);

        let r: serde_json::Value = serde_json::from_str(&handle_line(
            r#"{"v":1,"cmd":"set_config","size":-2.0}"#,
            &mut s,
        ))
        .unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(s.config.size, 3.0);

        // `3.5e38` is a valid JSON number that parses to a finite f64 but
        // overflows to `f32::INFINITY`; it must be rejected, not applied.
        let r: serde_json::Value = serde_json::from_str(&handle_line(
            r#"{"v":1,"cmd":"set_config","speed":3.5e38}"#,
            &mut s,
        ))
        .unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["error"], "speed must be a finite, non-negative number");
        assert_eq!(s.config.speed, 60.0);
    }

    #[test]
    fn socket_path_is_sock() {
        let p = socket_path().to_string_lossy().into_owned();
        assert!(p.ends_with(".sock"), "unexpected socket path: {p}");
    }

    #[test]
    fn spawn_server_serves_requests() {
        let path = std::env::temp_dir().join(format!("wlsnow-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let shared = Arc::new(Mutex::new(state()));
        spawn_server(path.clone(), Arc::clone(&shared));

        // Poll-connect: the server thread binds asynchronously.
        let mut stream = None;
        for _ in 0..100 {
            match UnixStream::connect(&path) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        let mut stream = stream.expect("server should have bound the socket");
        stream.write_all(b"{\"v\":1,\"cmd\":\"hello\"}\n").unwrap();
        stream
            .write_all(b"{\"v\":1,\"cmd\":\"set_config\",\"flake_count\":42}\n")
            .unwrap();

        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();

        reader.read_line(&mut line).unwrap();
        let hello: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(hello["ok"], true);

        line.clear();
        reader.read_line(&mut line).unwrap();
        let set: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(set["ok"], true);

        assert_eq!(shared.lock().unwrap().config.flake_count, 42);

        let _ = std::fs::remove_file(&path);
    }
}
