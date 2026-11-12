#![no_main]
use libfuzzer_sys::fuzz_target;
use trace_sonic::live::{self, io::PubSubParser, AsicNames, Filter, Input, Lookup, Source, Tracker};
use trace_sonic::logs::Logs;

/// A lookup that answers every question with the same fuzzed text.
struct Echo(String);

impl Lookup for Echo {
    fn hget(&mut self, _: u8, _: &str, _: &str) -> Option<String> {
        Some(self.0.clone())
    }
    fn keys(&mut self, _: u8, _: &str) -> Vec<String> {
        vec![self.0.clone()]
    }
}

// Arbitrary log bytes: evidence extraction and live-mode parsing must never panic.
fuzz_target!(|data: &[u8]| {
    let logs = Logs::from_bytes(Some(data), Some(data), Some(data));
    let _ = logs.evidence(&["VLAN_TABLE:Vlan8"], Some(&|r| r.attrs.contains(&"x")), &["Vlan8", "10.0.0.0/8"]);

    let text = String::from_utf8_lossy(data);
    let mut look = Echo(text.lines().next().unwrap_or("").to_string());
    let mut tracker = Tracker::new(Filter::default(), 1, AsicNames::default());
    let mut parser = PubSubParser::default();
    let _ = live::route_name(&text);
    for (i, line) in text.lines().enumerate() {
        let now = i as u64 * 7;
        if let Some((db, key, op)) = parser.feed(line) {
            if let Some(source) = Source::from_db_id(db) {
                let _ = tracker.on_input(now, &Input::Db { source, key, op }, &mut look);
            }
        }
        for source in [Source::Config, Source::Appl, Source::State, Source::Asic] {
            let input = Input::Db { source, key: line.to_string(), op: "hset".into() };
            let _ = tracker.on_input(now, &input, &mut look);
        }
        for source in [Source::SwssRec, Source::SairedisRec, Source::Syslog] {
            let _ = tracker.on_input(now, &Input::Log { source, line: line.to_string() }, &mut look);
        }
        let _ = tracker.on_tick(now);
    }
});
