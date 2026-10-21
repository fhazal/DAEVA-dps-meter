//! In-game event alerts from the Daeva bot's schedule feed.
//!
//! The feed is the `/api/overview` JSON served by the Daeva web site
//! (aion2.bajubesi.org), so the meter and the Discord bot always agree on
//! event times. This module only parses it and decides when to raise an
//! alert; fetching is done by the app.

use crate::json::{self, Obj, Value};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct Occurrence {
    pub id: String,
    pub name: String,
    pub short: String,
    pub emoji: String,
    pub starts: f64,
    pub ends: f64,
    /// e.g. "Portal open" and when it closes.
    pub phase: Option<(String, f64)>,
    /// Extra line shown under the title (source, server...).
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub key: String,
    pub id: String,
    pub title: String,
    pub body: String,
    pub emoji: String,
    /// "soon" or "start".
    pub kind: &'static str,
    pub starts: f64,
    pub raised: f64,
}

impl Alert {
    pub fn to_json(&self) -> Value {
        Obj::new()
            .set("key", self.key.as_str())
            .set("id", self.id.as_str())
            .set("title", self.title.as_str())
            .set("body", self.body.as_str())
            .set("emoji", self.emoji.as_str())
            .set("kind", self.kind)
            .set("starts", self.starts)
            .set("raised", self.raised)
            .build()
    }
}

/// Parses an ISO-8601 UTC timestamp such as `2026-10-17T12:00:00.000Z`
/// (also accepts `+00:00` and no fraction) into unix seconds.
pub fn parse_iso_utc(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i64> { s.get(a..z)?.parse().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut rest = &s[19..];
    let mut frac = 0.0;
    if let Some(r) = rest.strip_prefix('.') {
        let n = r.bytes().take_while(|c| c.is_ascii_digit()).count();
        frac = format!("0.{}", &r[..n]).parse().unwrap_or(0.0);
        rest = &r[n..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        o if o.len() == 6 && (o.starts_with('+') || o.starts_with('-')) => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            sign * (o[1..3].parse::<i64>().ok()? * 3600 + o[4..6].parse::<i64>().ok()? * 60)
        }
        _ => return None,
    };
    // Days from civil (Howard Hinnant).
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + h * 3600 + mi * 60 + se - offset) as f64 + frac)
}

/// Reads the occurrences out of a Daeva `/api/overview` body.
pub fn parse_overview(body: &str) -> Result<Vec<Occurrence>, String> {
    let v = json::parse(body).map_err(|e| format!("bad feed JSON: {e:?}"))?;
    let events = v.get("events").and_then(|e| e.as_arr()).ok_or("feed has no events list")?;
    let mut out = Vec::new();
    for ev in events {
        let id = ev.str_or("id", "");
        if id.is_empty() {
            continue;
        }
        let name = ev.str_or("name", id);
        for o in ev.get("occurrences").and_then(|x| x.as_arr()).map(|a| a.as_slice()).unwrap_or(&[]) {
            let (Some(starts), Some(ends)) = (
                o.get("startsAt").and_then(|x| x.as_str()).and_then(parse_iso_utc),
                o.get("endsAt").and_then(|x| x.as_str()).and_then(parse_iso_utc),
            ) else {
                continue;
            };
            let phase = o.get("phase").and_then(|p| {
                let label = p.get("label")?.as_str()?.to_string();
                let end = parse_iso_utc(p.get("endsAt")?.as_str()?)?;
                Some((label, end))
            });
            out.push(Occurrence {
                id: id.to_string(),
                name: name.to_string(),
                short: ev.str_or("shortName", name).to_string(),
                emoji: ev.str_or("emoji", "").to_string(),
                starts,
                ends,
                phase,
                note: None,
            });
        }
    }
    if let Some(g) = v.get("gartua").filter(|g| g.as_obj().is_some()) {
        if let Some(spawn) = g.get("spawnAt").and_then(|x| x.as_str()).and_then(parse_iso_utc) {
            let source = match g.str_or("source", "") {
                "aion2timers" => "estimate from Aion2Timers",
                "manual" => "estimate set by a Daeva member",
                _ => "estimate",
            };
            out.push(Occurrence {
                id: "gartua".into(),
                name: "Immortal Gartua".into(),
                short: "Gartua".into(),
                emoji: "🐲".into(),
                starts: spawn,
                ends: spawn + 15.0 * 60.0,
                phase: None,
                note: Some(format!("{} · {source}", g.str_or("server", "Beritra"))),
            });
        }
    }
    out.sort_by(|a, b| a.starts.total_cmp(&b.starts));
    Ok(out)
}

pub struct Notifier {
    /// Seconds before the start to raise the "soon" alert (0 = off).
    pub lead_secs: f64,
    /// Raise an alert when the event starts.
    pub at_start: bool,
    pub muted: HashSet<String>,
    fired: HashMap<String, f64>,
}

/// An event that started longer ago than this gets no "started" alert
/// (meter launched mid-event).
const START_GRACE: f64 = 90.0;

impl Notifier {
    pub fn new(lead_secs: f64, at_start: bool, muted: impl IntoIterator<Item = String>) -> Notifier {
        Notifier { lead_secs, at_start, muted: muted.into_iter().collect(), fired: HashMap::new() }
    }

    pub fn poll(&mut self, now: f64, occs: &[Occurrence]) -> Vec<Alert> {
        let mut out = Vec::new();
        for o in occs {
            if self.muted.contains(&o.id) {
                continue;
            }
            let mut fire = |kind: &'static str, title: String, body: String| {
                let key = format!("{}|{}|{kind}", o.id, o.starts as i64);
                if self.fired.contains_key(&key) {
                    return;
                }
                self.fired.insert(key.clone(), o.starts);
                out.push(Alert { key, id: o.id.clone(), title, body, emoji: o.emoji.clone(), kind, starts: o.starts, raised: now });
            };
            let until = o.starts - now;
            let note = o.note.clone().map(|n| format!(" · {n}")).unwrap_or_default();
            if self.lead_secs > 0.0 && until > 0.0 && until <= self.lead_secs {
                let mins = (until / 60.0).ceil().max(1.0) as i64;
                fire("soon", format!("{} in {mins} min", o.name), format!("Starts soon{note}"));
            } else if self.at_start && until <= 0.0 && -until < START_GRACE && now < o.ends.max(o.starts + 60.0) {
                let body = match &o.phase {
                    Some((label, end)) if *end > now => format!("{label} for {} min{note}", ((end - now) / 60.0).ceil() as i64),
                    _ if o.ends - o.starts > 90.0 => format!("Runs {} min{note}", ((o.ends - o.starts) / 60.0).round() as i64),
                    _ => format!("Now{note}"),
                };
                fire("start", format!("{} has started", o.name), body);
            }
        }
        self.fired.retain(|_, starts| now - *starts < 3600.0);
        out
    }
}

/// The next `n` occurrences that are not muted and not over yet.
/// What the next-events line shows: events running now that end within the
/// hour (soonest end first), then the next ones to start, and last the
/// long-running ones (Arena of Tactics is open for hours and would otherwise
/// keep a spot all day, hiding an invasion that just started).
pub fn upcoming<'a>(occs: &'a [Occurrence], now: f64, muted: &HashSet<String>, n: usize) -> Vec<&'a Occurrence> {
    let mut list: Vec<&Occurrence> = occs.iter().filter(|o| o.ends > now && !muted.contains(&o.id)).collect();
    let rank = |o: &Occurrence| -> (u8, f64) {
        if o.starts > now {
            (1, o.starts)
        } else if o.ends - now <= 3600.0 {
            (0, o.ends)
        } else {
            (2, o.ends)
        }
    };
    list.sort_by(|a, b| {
        let (ra, ka) = rank(a);
        let (rb, kb) = rank(b);
        ra.cmp(&rb).then(ka.total_cmp(&kb))
    });
    list.truncate(n);
    list
}

pub fn occurrence_json(o: &Occurrence, now: f64) -> Value {
    Obj::new()
        .set("id", o.id.as_str())
        .set("name", o.name.as_str())
        .set("short", o.short.as_str())
        .set("emoji", o.emoji.as_str())
        .set("starts", o.starts)
        .set("ends", o.ends)
        .set("active", o.starts <= now && now < o.ends)
        .set("note", o.note.clone())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"{"now":"2026-10-17T09:41:00.000Z","region":{"id":"asia","label":"Asia"},
      "events":[
        {"id":"spacetime_rift","name":"Spacetime Rift","shortName":"Rift","emoji":"🌀","description":"x","confidence":"high",
         "occurrences":[{"startsAt":"2026-10-17T11:00:00.000Z","endsAt":"2026-10-17T12:00:00.000Z","status":"upcoming",
           "phase":{"label":"Portal open","endsAt":"2026-10-17T11:10:00.000Z","active":false}}]},
        {"id":"shugo_festival","name":"Shugo Festival","shortName":"Shugo","emoji":"🎪","description":"x","confidence":"high",
         "occurrences":[{"startsAt":"2026-10-17T10:00:00.000Z","endsAt":"2026-10-17T10:10:00.000Z","status":"upcoming","phase":null}]}],
      "gartua":{"server":"Beritra","spawnAt":"2026-10-17T10:30:00.000Z","cycleHours":12,"source":"aion2timers","reportedAt":null},
      "status":null}"#;

    #[test]
    fn iso() {
        assert_eq!(parse_iso_utc("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_iso_utc("2026-10-17T09:41:00.000Z"), Some(1_792_230_060.0));
        assert_eq!(parse_iso_utc("2026-10-17T17:41:00+08:00"), Some(1_792_230_060.0));
        assert_eq!(parse_iso_utc("2026-10-17T09:41:00.500Z"), Some(1_792_230_060.5));
        assert_eq!(parse_iso_utc("nope"), None);
    }

    #[test]
    fn feed_and_alerts() {
        let occs = parse_overview(FEED).unwrap();
        assert_eq!(occs.len(), 3);
        assert_eq!(occs[0].id, "shugo_festival");
        assert_eq!(occs[1].id, "gartua");
        assert_eq!(occs[2].phase.as_ref().unwrap().0, "Portal open");
        let rift = parse_iso_utc("2026-10-17T11:00:00Z").unwrap();
        let mut n = Notifier::new(300.0, true, ["shugo_festival".to_string()]);
        assert!(n.poll(rift - 400.0, &occs).is_empty());
        let a = n.poll(rift - 290.0, &occs);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].title, "Spacetime Rift in 5 min");
        assert!(n.poll(rift - 200.0, &occs).is_empty(), "fires once");
        let s = n.poll(rift + 5.0, &occs);
        assert_eq!(s[0].title, "Spacetime Rift has started");
        assert_eq!(s[0].body, "Portal open for 10 min");
        // Started long ago when the meter launched: no alert.
        let mut late = Notifier::new(300.0, true, []);
        assert!(late.poll(rift + 600.0, &occs).iter().all(|a| a.id != "spacetime_rift"));
        // Gartua carries its source.
        let g = parse_iso_utc("2026-10-17T10:30:00Z").unwrap();
        let ga = Notifier::new(300.0, true, []).poll(g - 60.0, &occs);
        assert_eq!(ga[0].body, "Starts soon · Beritra · estimate from Aion2Timers");
        assert_eq!(upcoming(&occs, g - 60.0, &n.muted, 5).len(), 2);
    }

    #[test]
    fn next_events_line_prefers_what_matters_now() {
        let o = |id: &str, starts: f64, ends: f64| Occurrence { id: id.into(), name: id.into(), short: id.into(), emoji: String::new(), starts, ends, phase: None, note: None };
        let now = 10_000.0;
        let occs = vec![
            o("arena_of_tactics", now - 3600.0, now + 9000.0), // open for hours
            o("spacetime_rift", now - 1500.0, now + 1900.0),
            o("dimensional_invasion", now - 60.0, now + 1500.0), // just started
            o("shugo_festival", now + 1800.0, now + 2400.0),
        ];
        let ids: Vec<&str> = upcoming(&occs, now, &HashSet::new(), 3).iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, vec!["dimensional_invasion", "spacetime_rift", "shugo_festival"]);
    }
}
