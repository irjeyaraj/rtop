use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

/// A running embedded shell backed by a PTY, with an output buffer.
pub struct ShellSession {
    pub(crate) master: Box<dyn portable_pty::MasterPty + Send>,
    pub(crate) writer: Option<Box<dyn std::io::Write + Send>>, 
    pub(crate) child: Box<dyn portable_pty::Child + Send>,
    pub(crate) buf: Arc<Mutex<Vec<u8>>>,
}

impl ShellSession {
    pub fn spawn(rows: u16, cols: u16) -> Option<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .ok()?;
        let master = pair.master;
        let cmd_builder = {
            let (prog, args) = super::default_shell_and_args();
            let mut b = CommandBuilder::new(prog);
            for a in args { b.arg(a); }
            b.env("TERM", std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string()));
            b
        };
        let child = pair.slave.spawn_command(cmd_builder).ok()?;
        let writer = master.take_writer().ok();
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        // Reader thread to capture output
        {
            let mut reader = master.try_clone_reader().ok()?;
            let buf_clone = buf.clone();
            thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Ok(mut b) = buf_clone.lock() {
                                b.extend_from_slice(&chunk[..n]);
                                // Trim buffer if huge to prevent unbounded growth
                                if b.len() > 512 * 1024 {
                                    let keep = 256 * 1024;
                                    let start = b.len() - keep;
                                    b.drain(0..start);
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        Some(Self { master, writer, child, buf })
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        if let Some(w) = self.writer.as_mut() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    pub fn read_stripped_lines(&self, width: usize, height: usize) -> Vec<String> {
        use strip_ansi_escapes::strip;
        let data = self.buf.lock().ok().map(|b| b.clone()).unwrap_or_default();
        let stripped = strip(&data);
        let text = String::from_utf8_lossy(&stripped);
        let mut lines: Vec<String> = text.split_inclusive(['\n']).map(|s| s.to_string()).collect();
        if lines.is_empty() { lines.push(String::new()); }
        // Wrap very long lines to width best-effort
        let mut wrapped: Vec<String> = Vec::new();
        for l in lines {
            if width == 0 { wrapped.push(l); continue; }
            let mut cur = l;
            while cur.len() > width {
                wrapped.push(cur[..width].to_string());
                cur = cur[width..].to_string();
            }
            wrapped.push(cur);
        }
        // Keep only last height lines to fit viewport
        let len = wrapped.len();
        if height > 0 && len > height { wrapped[len - height..].to_vec() } else { wrapped }
    }

    pub fn terminate(&mut self) {
        let _ = self.child.kill();
    }

    pub fn is_exited(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(Some(_status)) => true,
            Ok(None) => false,
            Err(_e) => false,
        }
    }
}
