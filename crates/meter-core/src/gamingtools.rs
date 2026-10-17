//! Name lookups from the AION 2 Global Database (aion2.gaming.tools),
//! used with the site admin's permission. One page per unknown id, cached
//! forever by the app, credited in the UI.

pub const SITE: &str = "https://aion2.gaming.tools";

pub fn npc_url(template: u32) -> String {
    format!("{SITE}/npcs/{template}")
}

pub fn skill_url(key: u32) -> String {
    format!("{SITE}/skills/{key}")
}

/// Pulls the entry name out of a gaming.tools page.
/// Titles look like "Abandoned Daeva - NPCs - Cogni - AION 2 Global".
pub fn parse_name(html: &str, section: &str) -> Option<String> {
    let from_title = tag_text(html, "title").and_then(|t| {
        let t = decode_entities(&t);
        let marker = format!(" - {section} - ");
        let name = match t.find(&marker) {
            Some(i) => t[..i].to_string(),
            None if t.ends_with("AION 2 Global") => t.split(" - ").next()?.to_string(),
            None => return None,
        };
        Some(name)
    });
    let name = from_title.or_else(|| tag_text(html, "h1").map(|h| decode_entities(&strip_tags(&h))))?;
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = name.to_ascii_lowercase();
    if name.is_empty() || name.chars().count() > 80 || lower.contains("not found") || lower.starts_with("all aion 2") || lower == "404" {
        return None;
    }
    Some(name)
}

fn tag_text(html: &str, tag: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find(&format!("<{tag}"))?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find(&format!("</{tag}>"))?;
    Some(html[start..end].trim().to_string())
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in s.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(semi) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" | "#x27" => Some('\''),
            "nbsp" => Some(' '),
            _ if ent.starts_with("#x") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_pages() {
        let npc = r#"<html><head><meta charset="utf-8"><title>Abandoned Daeva - NPCs - Cogni - AION 2 Global</title></head><body><h1 class="x">Abandoned Daeva</h1></body></html>"#;
        assert_eq!(parse_name(npc, "NPCs").as_deref(), Some("Abandoned Daeva"));
        let skill = "<title>Earth&#39;s Retribution - Skills - Cleric - AION 2 Global</title>";
        assert_eq!(parse_name(skill, "Skills").as_deref(), Some("Earth's Retribution"));
        let h1only = "<title>x</title><h1><span>Black Claw</span> Manduri &amp; Co</h1>";
        assert_eq!(parse_name(h1only, "NPCs").as_deref(), Some("Black Claw Manduri & Co"));
        assert_eq!(parse_name("<title>All AION 2 Skills</title><h1>All AION 2 Skills</h1>", "Skills"), None);
        assert_eq!(parse_name("<title>Page not found - AION 2 Global</title>", "NPCs"), None);
        assert_eq!(npc_url(2400002), "https://aion2.gaming.tools/npcs/2400002");
    }
}

pub const NPCS_DATA: &str = "https://cdn-hosted.gaming.tools/aion2/data/en/npcs.d.json";
pub const SKILLS_DATA: &str = "https://cdn-hosted.gaming.tools/aion2/data/en/skills.d.json";

/// Expands a `devalue` payload (a flat JSON array where container members
/// are indices into that array) into a normal JSON value.
pub fn devalue(src: &str) -> Result<crate::json::Value, String> {
    use crate::json::Value;
    let v = crate::json::parse(src).map_err(|e| format!("bad devalue JSON: {e:?}"))?;
    let arr = v.as_arr().ok_or("devalue root is not an array")?;
    if arr.is_empty() {
        return Err("empty devalue payload".into());
    }
    fn go(arr: &[Value], i: i64, depth: u32) -> Value {
        if i < 0 || i as usize >= arr.len() || depth > 64 {
            return Value::Null; // -1 undefined, -2 hole, -3 NaN... all treated as null
        }
        match &arr[i as usize] {
            Value::Arr(items) => {
                // ["Date", ...] style tagged values carry a string first.
                if let Some(Value::Str(_)) = items.first() {
                    return Value::Null;
                }
                Value::Arr(items.iter().map(|x| x.as_f64().map_or(Value::Null, |j| go(arr, j as i64, depth + 1))).collect())
            }
            Value::Obj(o) => {
                let mut out = crate::json::Obj::new();
                for (k, x) in o {
                    out.put(k, x.as_f64().map_or(Value::Null, |j| go(arr, j as i64, depth + 1)));
                }
                out.build()
            }
            other => other.clone(),
        }
    }
    Ok(go(arr, 0, 0))
}

/// Where gaming.tools serves its images; `iconPath` values are relative to it.
pub const IMAGES: &str = "https://cdn-hosted.gaming.tools/aion2";

/// One entry of a gaming.tools data file.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: u32,
    pub name: String,
    pub icon: Option<String>,
}

/// Entries from a gaming.tools `*.d.json` data file.
pub fn entries_from_data(src: &str) -> Result<Vec<Entry>, String> {
    let root = devalue(src)?;
    let list = root.as_arr().ok_or("data root is not a list")?;
    let mut out = Vec::new();
    for r in list {
        let id = r.get("id").and_then(|x| x.as_str().and_then(|s| s.parse::<u32>().ok()).or_else(|| x.as_u64().map(|n| n as u32)));
        let name = r.get("name").and_then(|x| x.as_str()).map(|s| s.trim().to_string());
        let icon = r.get("iconPath").and_then(|x| x.as_str()).filter(|p| p.starts_with('/') && !p.contains("..") && p.len() < 200).map(String::from);
        if let (Some(id), Some(name)) = (id, name) {
            if !name.is_empty() {
                out.push(Entry { id, name, icon });
            }
        }
    }
    if out.is_empty() {
        return Err("no names in data file".into());
    }
    Ok(out)
}

/// `(id, name)` pairs from a gaming.tools `*.d.json` data file.
pub fn names_from_data(src: &str) -> Result<Vec<(u32, String)>, String> {
    Ok(entries_from_data(src)?.into_iter().map(|e| (e.id, e.name)).collect())
}

#[cfg(test)]
mod devalue_tests {
    use super::*;

    #[test]
    fn npc_file_shape() {
        let src = r#"[[1,14],{"entityType":2,"categories":3,"aliases":6,"minimumLevel":8,"maximumLevel":9,"level":10,"mainCategoryId":11,"name":7,"iconPath":12,"id":13},"npc",[4,5],"npcs/monster/intellect","npcs/citizen",[7,7,7,7],"Draconute Ranger",17,70,0,"npcs","/images/custom/npcs.2000002.webp","2000002",{"name":15,"id":16,"x":-1},"Earth's Retribution","17010000"]"#;
        let n = names_from_data(src).unwrap();
        assert_eq!(n, vec![(2000002, "Draconute Ranger".to_string()), (17010000, "Earth's Retribution".to_string())]);
        assert_eq!(entries_from_data(src).unwrap()[0].icon.as_deref(), Some("/images/custom/npcs.2000002.webp"));
        let v = devalue(src).unwrap();
        assert_eq!(v.as_arr().unwrap()[0].get("aliases").unwrap().as_arr().unwrap().len(), 4);
    }
}
