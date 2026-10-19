use super::{
    array, num,
    paths::{read_json, Paths},
    private,
    render::text::{fill_levels, one_line},
    string,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub fn notice_life() -> f64 {
    std::env::var("CSR_CARD_LIFE_S")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(300) as f64
}
pub fn waits_on_press(s: &Value) -> bool {
    s["star"] == true || s["live"] == true || matches!(string(&s["name"]), "lost" | "earlier")
}
pub fn timed(s: &Value) -> bool {
    s["name"] != "story" && !waits_on_press(s)
}
pub fn life_share(s: &Value, now: f64) -> Option<f64> {
    let born = s["born"].as_f64()?;
    timed(s).then(|| ((born + notice_life() - now) / notice_life()).clamp(0., 1.))
}
pub fn living(sections: Vec<Value>, now: f64) -> Vec<Value> {
    sections
        .into_iter()
        .filter_map(|mut s| {
            if let Some(share) = life_share(&s, now) {
                if share <= 0. {
                    return None;
                }
                s["life"] = json!(share);
            }
            Some(s)
        })
        .collect()
}
pub fn signature(s: &Value) -> String {
    let said: Vec<_> = array(&s["items"])
        .iter()
        .filter(|i| i["unsigned"] != true && !string(&i["key"]).starts_with("fold:"))
        .map(|i| {
            json!([
                i["key"],
                i["title"].as_str().unwrap_or(""),
                i["open_title"].as_str().unwrap_or("")
            ])
        })
        .collect();
    // Python json.dumps uses spaces after both separators, without ASCII escaping.
    let words = said
        .iter()
        .map(|r| {
            format!(
                "[{}]",
                array(r)
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Sha256::digest(format!("[{words}]"))
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn age_of(seconds: f64) -> String {
    let minutes = (seconds.max(0.) / 60.).floor() as u64;
    if minutes < 1 {
        "now".into()
    } else if minutes < 120 {
        format!("{minutes}m")
    } else if minutes < 2880 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}d", minutes / 1440)
    }
}
pub struct Feed {
    entries: BTreeMap<String, Value>,
    primed: bool,
    saved: f64,
    dirty: bool,
    paths: Paths,
    sid: String,
}
impl Feed {
    pub fn new(paths: &Paths, sid: &str) -> Self {
        let got = paths
            .kept_file("seen", sid)
            .map(|p| read_json(&p))
            .unwrap_or(Value::Null);
        let entries = got["sections"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, e)| e["sig"].is_string() && e["since"].is_number())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<BTreeMap<_, _>>();
        Self {
            primed: !entries.is_empty(),
            entries,
            saved: 0.,
            dirty: false,
            paths: paths.clone(),
            sid: sid.into(),
        }
    }
    pub fn observe(&mut self, sections: &[Value], now: f64) {
        for s in sections {
            let name = string(&s["name"]);
            let sig = signature(s);
            if self.entries.get(name).is_none_or(|e| e["sig"] != sig) {
                self.entries.insert(name.into(),json!({"sig":sig,"since":now,"seen":null,"back":null,"new":if self.primed{json!(now)}else{Value::Null},"at":null}));
                self.dirty = true;
            }
            let e = self.entries.get_mut(name).unwrap();
            e["at"] = s["at"].clone();
            e["born"] = s
                .get("born")
                .unwrap_or(&s["at"])
                .as_f64()
                .filter(|n| *n != 0.)
                .map(|n| json!(n))
                .unwrap_or(e["since"].clone());
            e["timed"] = json!(timed(s));
            e["active"] = json!(s["active"] == true);
        }
        self.primed = true;
    }
    fn when(&self, name: &str) -> f64 {
        let e = &self.entries[name];
        e["at"]
            .as_f64()
            .filter(|n| *n != 0.)
            .unwrap_or(num(&e["since"]))
    }
    fn fresh_at(&self, name: &str) -> f64 {
        self.when(name).max(num(&self.entries[name]["new"]))
    }
    pub fn sunk(&self, name: &str, now: f64) -> bool {
        let Some(e) = self.entries.get(name) else {
            return false;
        };
        let Some(seen) = e["seen"].as_f64() else {
            return false;
        };
        name != "story"
            && e["timed"] != true
            && e["active"] != true
            && (now - seen >= 180. || (e["back"].is_null() && now - self.when(name) > 2700.))
    }
    pub fn is_new(&self, name: &str, now: f64) -> bool {
        self.entries
            .get(name)
            .and_then(|e| e["new"].as_f64())
            .is_some_and(|t| t != 0. && now - t < 60.)
    }
    pub fn page(&mut self, mut page: Value, now: f64) -> Value {
        let live: Vec<_> = array(&page["sections"])
            .iter()
            .filter(|s| !array(&s["items"]).is_empty())
            .cloned()
            .collect();
        self.observe(&live, now);
        let live = living(
            live.into_iter()
                .map(|mut s| {
                    s["born"] = self.entries[string(&s["name"])]["born"].clone();
                    s
                })
                .collect(),
            now,
        );
        let mut pinned = vec![];
        let mut fresh = vec![];
        let mut gone = vec![];
        for s in live {
            if s["name"] == "story" {
                pinned.push(s);
            } else if self.sunk(string(&s["name"]), now) {
                gone.push(s);
            } else {
                fresh.push(s);
            }
        }
        fresh.sort_by(|a, b| {
            let rank = |s: &Value| {
                let n = string(&s["name"]);
                (num(&self.entries[n]["back"]), self.fresh_at(n))
            };
            rank(b)
                .partial_cmp(&rank(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        gone.sort_by(|a, b| {
            self.fresh_at(string(&b["name"]))
                .total_cmp(&self.fresh_at(string(&a["name"])))
        });
        let nothing = fresh.is_empty();
        pinned.extend(fresh);
        for s in &mut pinned {
            if self.is_new(string(&s["name"]), now) {
                let text = string(&s["right"]);
                s["right"] = json!(if text.is_empty() {
                    "new".into()
                } else {
                    format!("new · {text}")
                });
            }
        }
        if !gone.is_empty() {
            let items:Vec<_>=gone.iter().map(|s|{let first=array(&s["items"]).iter().find(|i|!string(&i["key"]).starts_with("fold:")).unwrap_or(&s["items"][0]);let said=if s["waits"]==true{"waits on you"}else{string(&s["brief"])};let title=string(&first["title"]).replace('`',"");let age=age_of(now-self.when(string(&s["name"])));let parts=[string(&s["title"]),said,&age,if title==said{""}else{&title}];json!({"key":format!("back:{}",s["name"].as_str().unwrap()),"title":one_line(&parts.into_iter().filter(|p|!p.is_empty()).collect::<Vec<_>>().join(" · "),300),"lines":1,"quiet":true,"back":s["name"],"send":"","detail":[]})}).collect();
            let forms = if nothing {
                vec![
                    "nothing new · click one to bring it back",
                    "nothing new · click to bring back",
                    "nothing new",
                ]
            } else {
                vec!["click one to bring it back", "click to bring back"]
            };
            pinned.push(json!({"name":"earlier","title":"Earlier","right":forms[0],"rights":&forms[1..],"items":items}));
        }
        pinned.extend(
            array(&page["sections"])
                .iter()
                .filter(|s| array(&s["items"]).is_empty())
                .cloned(),
        );
        page["sections"] = json!(pinned);
        page
    }
    pub fn shown(&mut self, names: &[String], now: f64) {
        for name in names {
            if let Some(e) = self.entries.get_mut(name) {
                if e["seen"].is_null() {
                    e["seen"] = json!(now);
                    self.dirty = true;
                }
            }
        }
    }
    pub fn back(&mut self, name: &str, now: f64) {
        if let Some(e) = self.entries.get_mut(name) {
            e["back"] = json!(now);
            e["seen"] = Value::Null;
            self.dirty = true;
        }
    }
    pub fn save(&mut self, now: f64) {
        if !self.dirty || now - self.saved < 30. {
            return;
        }
        let Some(path) = self.paths.kept_file("seen", &self.sid) else {
            return;
        };
        if private::write_atomic(
            &path,
            &private::dumps(&json!({"sections":self.entries})),
            &self.paths.keep,
        )
        .is_ok()
        {
            self.saved = now;
            self.dirty = false;
        }
    }
    pub fn life_level(&self, name: &str, now: f64) -> Option<i32> {
        let e = self.entries.get(name)?;
        if e["timed"] != true {
            return None;
        }
        let share = life_share(&json!({"name":name,"born":e["born"]}), now)?;
        Some(if share > 0. {
            fill_levels(share, 1)[0] as i32
        } else {
            -1
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_a_notice_leaves_on_its_own_clock() {
        let s = json!({"name":"rules","born":1000});
        assert_eq!(life_share(&s, 1150.), Some(0.5));
        assert!(living(vec![s], 1300.).is_empty());
    }
    #[test]
    fn test_the_story_never_sinks() {
        let d = tempfile::tempdir().unwrap();
        let mut f = Feed::new(&Paths::new(d.path().into()), "s");
        f.observe(
            &[json!({"name":"story","items":[{"key":"x","title":"hi"}],"at":100})],
            1000.,
        );
        f.shown(&["story".into()], 1000.);
        assert!(!f.sunk("story", 5000.));
    }
    #[test]
    fn test_a_waiting_card_moves_to_earlier_and_a_click_brings_it_back() {
        let d = tempfile::tempdir().unwrap();
        let mut f = Feed::new(&Paths::new(d.path().into()), "s");
        let p = json!({"sections":[{"name":"dreams","title":"What if…","right":"4 left","items":[{"key":"d","title":"What if?"}],"star":true,"at":1000}]});
        f.page(p.clone(), 1000.);
        f.shown(&["dreams".into()], 1000.);
        assert_eq!(f.page(p.clone(), 1181.)["sections"][0]["name"], "earlier");
        f.back("dreams", 1182.);
        assert_eq!(f.page(p, 1182.)["sections"][0]["name"], "dreams");
    }
}
