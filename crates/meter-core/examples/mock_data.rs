//! Dumps simulator output as JSON for UI previews without the Windows app.
use meter_core::engine::{Engine, EngineConfig, Names};
use meter_core::json::{Obj, Value};
use meter_core::sim::{Simulator, DEMO_NAMES};

fn main() {
    let mut e = Engine::new(EngineConfig::default(), Names::from_json(DEMO_NAMES).unwrap());
    let mut s = Simulator::new(42);
    let mut t = 1_760_000_000.0;
    let end = t + 95.0;
    while t < end {
        for ev in s.step(t) {
            e.handle(&ev);
        }
        e.tick(t);
        t += 0.25;
    }
    let live = e.current.as_ref().or(e.history.back()).unwrap();
    let table = e.fight_table(live, None);
    let detail = e.actor_detail(live, 1001).unwrap();
    let past = e.history.back().unwrap();
    let out = Obj::new()
        .set("table", table)
        .set("detail", detail)
        .set("history", e.history_list())
        .set("record", e.fight_record(past))
        .build();
    println!("{}", out.to_string());
    let _ = Value::Null;
}
