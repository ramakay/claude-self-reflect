//! AST-grep demo: tests code-aware search features added in Phase 4.
//!
//! After importing this conversation, searching for "dispatch_events"
//! or "EventProcessor" should find this session via AST analysis.

use std::collections::HashMap;

/// A sample struct to verify AST extraction picks up type definitions.
pub struct EventProcessor {
    handlers: HashMap<String, Box<dyn Fn(&str)>>,
    event_count: usize,
    is_running: bool,
}

impl EventProcessor {
    /// Constructor — AST analysis should extract this as a function definition.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            event_count: 0,
            is_running: false,
        }
    }

    /// Register an event handler — tests function parameter extraction.
    pub fn register_handler(&mut self, event_type: &str, handler: Box<dyn Fn(&str)>) {
        self.handlers.insert(event_type.to_string(), handler);
    }

    /// Dispatch events — the key function name we'll search for after restart.
    pub fn dispatch_events(&mut self, events: &[(&str, &str)]) -> Result<usize, String> {
        if !self.is_running {
            return Err("Processor not started".to_string());
        }

        let mut processed = 0;
        for (event_type, payload) in events {
            if let Some(handler) = self.handlers.get(*event_type) {
                handler(payload);
                processed += 1;
            }
        }
        self.event_count += processed;
        Ok(processed)
    }

    /// Start the processor.
    pub fn start(&mut self) {
        self.is_running = true;
    }

    /// Get total events processed.
    pub fn total_events(&self) -> usize {
        self.event_count
    }
}

fn main() {
    let mut processor = EventProcessor::new();
    processor.register_handler("click", Box::new(|payload| println!("Click: {}", payload)));
    processor.start();

    let events = vec![("click", "button_1"), ("click", "button_2")];
    match processor.dispatch_events(&events) {
        Ok(count) => println!("Processed {} events", count),
        Err(e) => eprintln!("Error: {}", e),
    }
}
