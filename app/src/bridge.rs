//! Client for the Kotlin parser helper (`kotatsu-bridge`), spoken over
//! line-delimited JSON on the child's stdin/stdout.

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub type RpcResult = Result<Value, String>;

/// A response slot that the UI polls each frame.
#[derive(Clone)]
pub struct Pending {
    id: u64,
    slot: Arc<OnceLock<RpcResult>>,
}

impl Pending {
    pub fn ready(&self) -> Option<&RpcResult> {
        self.slot.get()
    }
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Typed view of a finished result. `None` while still running.
    pub fn get<T: DeserializeOwned>(&self) -> Option<Result<T, String>> {
        self.slot.get().map(|r| match r {
            Ok(v) => T::deserialize(v).map_err(|e| format!("bad response: {e}")),
            Err(e) => Err(e.clone()),
        })
    }
    pub fn failed(msg: String) -> Pending {
        let slot = Arc::new(OnceLock::new());
        let _ = slot.set(Err(msg));
        Pending { id: 0, slot }
    }
}

type Waiters = Arc<Mutex<HashMap<u64, (Arc<OnceLock<RpcResult>>, Option<Box<dyn FnOnce() + Send>>)>>>;

pub struct Bridge {
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    next: AtomicU64,
    waiters: Waiters,
    repaint: Box<dyn Fn() + Send + Sync>,
    pub error: Mutex<Option<String>>,
}

fn find_helper() -> Option<(PathBuf, Vec<String>)> {
    if let Ok(p) = std::env::var("KOTATSU_BRIDGE") {
        let p = PathBuf::from(p);
        return Some(if p.extension().is_some_and(|e| e == "jar") {
            ("java".into(), vec!["-jar".into(), p.to_string_lossy().into()])
        } else {
            (p, vec![])
        });
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for cand in [
        exe_dir.join("kotatsu-bridge"),
        exe_dir.join("../lib/kotatsu-desktop/kotatsu-bridge"),
        exe_dir.join("../../../bridge/build/native/kotatsu-bridge"),
    ] {
        if cand.is_file() {
            return Some((cand, vec![]));
        }
    }
    for cand in [exe_dir.join("kotatsu-bridge-all.jar"), exe_dir.join("../../../bridge/build/libs/kotatsu-bridge-all.jar")] {
        if cand.is_file() {
            return Some(("java".into(), vec!["-XX:+UseSerialGC".into(), "-Xss1m".into(), "-jar".into(), cand.to_string_lossy().into()]));
        }
    }
    None
}

impl Bridge {
    pub fn start(data_dir: PathBuf, repaint: impl Fn() + Send + Sync + 'static) -> Arc<Bridge> {
        let bridge = Arc::new(Bridge {
            stdin: Mutex::new(None),
            child: Mutex::new(None),
            next: AtomicU64::new(1),
            waiters: Default::default(),
            repaint: Box::new(repaint),
            error: Mutex::new(None),
        });
        let Some((prog, mut args)) = find_helper() else {
            *bridge.error.lock().unwrap() = Some("kotatsu-bridge helper not found (set KOTATSU_BRIDGE)".into());
            return bridge;
        };
        args.push(data_dir.to_string_lossy().into());
        let spawned = Command::new(&prog)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                *bridge.error.lock().unwrap() = Some(format!("failed to start {}: {e}", prog.display()));
                return bridge;
            }
        };
        let stdout = child.stdout.take().unwrap();
        *bridge.stdin.lock().unwrap() = child.stdin.take();
        *bridge.child.lock().unwrap() = Some(child);

        let weak = Arc::downgrade(&bridge);
        std::thread::Builder::new()
            .name("bridge-rx".into())
            .spawn(move || {
                let reader = BufReader::with_capacity(1 << 16, stdout);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    let Some(b) = weak.upgrade() else { break };
                    let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                    let id = v.get("id").and_then(Value::as_u64).unwrap_or(0);
                    let res = match v.get("e").and_then(Value::as_str) {
                        Some(e) => Err(e.to_string()),
                        None => Ok(v.get("r").cloned().unwrap_or(Value::Null)),
                    };
                    let w = b.waiters.lock().unwrap().remove(&id);
                    if let Some((slot, cb)) = w {
                        let _ = slot.set(res);
                        if let Some(cb) = cb {
                            cb();
                        }
                    }
                    (b.repaint)();
                }
                if let Some(b) = weak.upgrade() {
                    *b.error.lock().unwrap() = Some("parser helper exited".into());
                    for (_, (slot, cb)) in b.waiters.lock().unwrap().drain() {
                        let _ = slot.set(Err("parser helper exited".into()));
                        if let Some(cb) = cb {
                            cb();
                        }
                    }
                    (b.repaint)();
                }
            })
            .unwrap();
        bridge
    }

    fn send(&self, id: u64, method: &str, params: Value) -> bool {
        let mut line = serde_json::to_vec(&json!({"id": id, "m": method, "p": params})).unwrap();
        line.push(b'\n');
        let mut g = self.stdin.lock().unwrap();
        match g.as_mut() {
            Some(s) => s.write_all(&line).and_then(|_| s.flush()).is_ok(),
            None => false,
        }
    }

    pub fn call(&self, method: &str, params: Value) -> Pending {
        self.call_then(method, params, None)
    }

    /// Like `call`, but runs `done` on the reader thread when the result lands.
    pub fn call_then(&self, method: &str, params: Value, done: Option<Box<dyn FnOnce() + Send>>) -> Pending {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(OnceLock::new());
        self.waiters.lock().unwrap().insert(id, (slot.clone(), done));
        if !self.send(id, method, params) {
            if let Some((slot, cb)) = self.waiters.lock().unwrap().remove(&id) {
                let msg = self.error.lock().unwrap().clone().unwrap_or_else(|| "parser helper not running".into());
                let _ = slot.set(Err(msg));
                if let Some(cb) = cb {
                    cb();
                }
            }
        }
        Pending { id, slot }
    }

    /// Blocking call, for worker threads.
    pub fn call_blocking(&self, method: &str, params: Value) -> RpcResult {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let p = self.call_then(method, params, Some(Box::new(move || {
            let _ = tx.send(());
        })));
        let _ = rx.recv();
        p.ready().cloned().unwrap_or_else(|| Err("no response".into()))
    }

    pub fn cancel(&self, p: &Pending) {
        if p.ready().is_none() && p.id != 0 {
            self.send(0, "cancel", json!({"id": p.id}));
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // Closing stdin makes the helper flush cookies and exit.
        self.stdin.lock().unwrap().take();
        if let Some(mut c) = self.child.lock().unwrap().take() {
            for _ in 0..20 {
                if let Ok(Some(_)) = c.try_wait() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            let _ = c.kill();
        }
    }
}
