//! 日志

use std::io::{self, Write};

use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub fn init() {
    // wasm 上 panic 直接 abort,宿主只能看到回溯;把消息打到 stderr 便于定位
    std::panic::set_hook(Box::new(|info| {
        eprintln!("[Plugin] PANIC: {info}");
    }));

    let writer = move || PluginWriter(io::stdout());
    let console_layer = fmt::layer()
        .with_target(true)
        .with_ansi(true)
        .with_file(true)
        .with_line_number(true)
        .with_writer(writer)
        .compact();

    let _ = tracing_subscriber::registry()
        .with(console_layer)
        .try_init();
}

struct PluginWriter<W: Write>(W);

impl<W: Write> Write for PluginWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_all(b"[Plugin] ")?;
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
