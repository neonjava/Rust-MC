//! The server console: lines typed on standard input become commands.
//!
//! Why: someone has to be able to run commands before any operator exists,
//! and to stop the server cleanly.
//! Done: one thread reading lines and handing them to the game loop.
//! Not done: line editing and tab completion.

use std::io::BufRead;
use std::sync::mpsc::SyncSender;
use std::thread;

use super::connection::Event;

/// Starts the console thread. It ends at end of input or when the game loop
/// is gone; the queue it sends to is the game loop's bounded inbox.
pub fn spawn(events: SyncSender<Event>) -> std::io::Result<()> {
    thread::Builder::new()
        .name("console".to_owned())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else {
                    return;
                };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if events.send(Event::Console(line.to_owned())).is_err() {
                    return;
                }
            }
        })
        .map(|_| ())
}
