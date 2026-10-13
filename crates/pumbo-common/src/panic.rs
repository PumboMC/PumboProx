//! Panic messages through the host logger.
//!
//! In WebAssembly a panic ends as a trap ("unreachable") and stderr is thrown
//! away, so without a hook nobody sees where it happened.

/// Sends every panic message with its location to `log`. Call it first in the
/// plugin's load or init. The hook takes no locks; it formats one line.
pub fn install(log: fn(&str)) {
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.payload_as_str().unwrap_or("(no message)");
        match info.location() {
            Some(at) => log(&format!("panic at {}:{}: {msg}", at.file(), at.line())),
            None => log(&format!("panic: {msg}")),
        }
    }));
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    static SEEN: Mutex<String> = Mutex::new(String::new());

    fn record(line: &str) {
        if let Ok(mut s) = SEEN.lock() {
            s.push_str(line);
            s.push('\n');
        }
    }

    #[test]
    fn panic_reaches_the_logger() {
        // The hook is process-wide; parallel tests may panic into it, so restore it quickly.
        let prev = std::panic::take_hook();
        super::install(record);
        let _ = std::panic::catch_unwind(|| panic!("boom {}", 7));
        std::panic::set_hook(prev);
        let seen = SEEN.lock().unwrap().clone();
        let line = seen.lines().find(|l| l.contains(": boom 7")).unwrap_or_else(|| panic!("{seen}"));
        assert!(line.starts_with("panic at ") && line.contains("panic.rs:"), "{line}");
    }
}
