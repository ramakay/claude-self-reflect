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

fn notice_life_value(raw: &str) -> f64 {
    let raw = raw.trim();
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return 300.;
    }
    raw.parse::<u32>().ok().filter(|n| *n > 0).unwrap_or(300) as f64
}
pub fn notice_life() -> f64 {
    notice_life_value(&std::env::var("CSR_CARD_LIFE_S").unwrap_or_default())
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
                i["open_title"].as_str().unwrap_or(""),
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
                self.entries.insert(
                    name.into(),
                    json!({
                        "sig": sig,
                        "since": now,
                        "seen": null,
                        "back": null,
                        "new": if self.primed {
                            json!(now)
                        } else {
                            Value::Null
                        },
                        "at": null,
                    }),
                );
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
                let mut forms = vec![string(&s["right"]).to_owned()];
                forms.extend(array(&s["rights"]).iter().map(|v| string(v).to_owned()));
                let mut forms: Vec<_> = forms
                    .into_iter()
                    .map(|f| {
                        if f.is_empty() {
                            "new".to_owned()
                        } else {
                            format!("new · {f}")
                        }
                    })
                    .collect();
                s["right"] = json!(forms.remove(0));
                forms.push("new".into());
                s["rights"] = json!(forms);
            }
        }
        if !gone.is_empty() {
            let items: Vec<_> = gone
                .iter()
                .map(|s| {
                    let first = array(&s["items"])
                        .iter()
                        .find(|i| !string(&i["key"]).starts_with("fold:"))
                        .unwrap_or(&s["items"][0]);
                    let said = if s["waits"] == true {
                        "waits on you"
                    } else {
                        string(&s["brief"])
                    };
                    let title = if string(&first["key"]).starts_with("fold:") {
                        string(&first["title"]).chars().skip(2).collect::<String>()
                    } else {
                        string(&first["title"]).to_owned()
                    }
                    .replace('`', "");
                    let age = age_of(now - self.when(string(&s["name"])));
                    let parts = [
                        string(&s["title"]),
                        said,
                        &age,
                        if title == said { "" } else { &title },
                    ];
                    json!({
                        "key": format!("back:{}", s["name"].as_str().unwrap()),
                        "title": one_line(
                            &parts
                                .into_iter()
                                .filter(|p| !p.is_empty())
                                .collect::<Vec<_>>()
                                .join(" · "),
                            300,
                        ),
                        "lines": 1,
                        "quiet": true,
                        "back": s["name"],
                        "send": "",
                        "detail": [],
                    })
                })
                .collect();
            let forms = if nothing {
                vec![
                    "nothing new · click one to bring it back",
                    "nothing new · click to bring back",
                    "nothing new",
                ]
            } else {
                vec!["click one to bring it back", "click to bring back"]
            };
            pinned.push(json!({
                "name": "earlier",
                "title": "Earlier",
                "right": forms[0],
                "rights": &forms[1..],
                "items": items,
            }));
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
            &private::dumps(&json!({
                "sections": self.entries,
            })),
            &self.paths.keep,
        )
        .is_ok()
        {
            self.saved = now;
            self.dirty = false;
        }
    }
    pub fn key(&self, now: f64) -> Vec<(String, bool, bool, Option<i32>)> {
        self.entries
            .keys()
            .map(|name| {
                (
                    name.clone(),
                    self.sunk(name, now),
                    self.is_new(name, now),
                    self.life_level(name, now),
                )
            })
            .collect()
    }
    pub fn life_level(&self, name: &str, now: f64) -> Option<i32> {
        let e = self.entries.get(name)?;
        if e["timed"] != true {
            return None;
        }
        let share = life_share(
            &json!({
                "name": name,
                "born": e["born"],
            }),
            now,
        )?;
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
        let s = json!({
            "name": "rules",
            "born": 1000,
        });
        assert_eq!(life_share(&s, 1150.), Some(0.5));
        assert!(living(vec![s], 1300.).is_empty());
    }
    #[test]
    fn test_the_story_never_sinks() {
        let d = tempfile::tempdir().unwrap();
        let mut f = Feed::new(&Paths::new(d.path().into()), "s");
        f.observe(
            &[json!({
                "name": "story",
                "items": [{
                    "key": "x",
                    "title": "hi",
                }],
                "at": 100,
            })],
            1000.,
        );
        f.shown(&["story".into()], 1000.);
        assert!(!f.sunk("story", 5000.));
    }
    #[test]
    fn test_a_waiting_card_moves_to_earlier_and_a_click_brings_it_back() {
        let d = tempfile::tempdir().unwrap();
        let mut f = Feed::new(&Paths::new(d.path().into()), "s");
        let p = json!({
            "sections": [{
                "name": "dreams",
                "title": "What if…",
                "right": "4 left",
                "items": [{
                    "key": "d",
                    "title": "What if?",
                }],
                "star": true,
                "at": 1000,
            }],
        });
        f.page(p.clone(), 1000.);
        f.shown(&["dreams".into()], 1000.);
        assert_eq!(f.page(p.clone(), 1181.)["sections"][0]["name"], "earlier");
        f.back("dreams", 1182.);
        assert_eq!(f.page(p, 1182.)["sections"][0]["name"], "dreams");
    }
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    const NOW: f64 = 1800000000.;
    fn sect(name: &str, title: &str, at: Option<f64>) -> Value {
        let mut c = name.chars();
        let title_name = c.next().unwrap().to_uppercase().collect::<String>() + c.as_str();
        json!({
            "name": name,
            "title": title_name,
            "right": "",
            "at": at,
            "items": [{
                "key": format!("{name}:0"),
                "title": title,
                "lines": 1,
            }],
        })
    }
    fn page(sections: Vec<Value>) -> Value {
        json!({
            "head": {},
            "quiet": "",
            "sections": sections,
        })
    }
    fn names(p: &Value) -> Vec<&str> {
        array(&p["sections"])
            .iter()
            .filter(|s| !array(&s["items"]).is_empty())
            .map(|s| string(&s["name"]))
            .collect()
    }
    fn setup() -> (tempfile::TempDir, Feed) {
        let d = tempfile::tempdir().unwrap();
        let f = Feed::new(&Paths::new(d.path().into()), "sess-feed");
        (d, f)
    }
    #[test]
    fn test_the_signature_is_what_a_section_holds_never_a_fold_a_click_opened() {
        let base = sect("dreams", "What if A?", None);
        let mut changed = base.clone();
        changed["right"] = json!("3 left");
        assert_eq!(signature(&base), signature(&changed));
        changed["items"][0]["title"] = json!("What if B?");
        assert_ne!(signature(&base), signature(&changed));
        let mut folded = base.clone();
        folded["items"].as_array_mut().unwrap().extend([
            json!({
                "key": "fold:passed",
                "title": "⌄ 1 passed",
            }),
            json!({
                "key": "dream:x",
                "title": "old",
                "unsigned": true,
            }),
        ]);
        assert_eq!(signature(&base), signature(&folded));
    }
    #[test]
    fn test_the_story_is_pinned_then_newest_content_first_and_a_tie_keeps_the_page_order() {
        let (_d, mut f) = setup();
        let p = page(vec![
            sect("story", "S", Some(NOW - 600.)),
            sect("lost", "L", Some(NOW - 60.)),
            sect("coach", "C", None),
            sect("rules", "R", None),
        ]);
        let got = f.page(p, NOW);
        assert_eq!(names(&got), ["story", "coach", "rules", "lost"]);
        assert!(array(&got["sections"])
            .iter()
            .all(|s| !string(&s["right"]).contains("new")));
    }
    #[test]
    fn test_a_seen_card_moves_to_earlier_but_an_unseen_card_and_the_story_stay() {
        let (_d, mut f) = setup();
        let mut d = sect("dreams", "What if A?", Some(NOW - 60.));
        d["star"] = json!(true);
        d["waits"] = json!(true);
        let p = page(vec![
            sect("story", "S", Some(NOW - 60.)),
            d,
            sect("lost", "L", Some(NOW - 30.)),
        ]);
        f.page(p.clone(), NOW);
        f.shown(&["story".into(), "dreams".into()], NOW);
        assert_eq!(
            names(&f.page(p.clone(), NOW + 179.)),
            ["story", "lost", "dreams"]
        );
        let got = f.page(p.clone(), NOW + 180.);
        assert_eq!(names(&got), ["story", "lost", "earlier"]);
        assert_eq!(
            got["sections"][2]["items"][0]["title"],
            "Dreams · waits on you · 4m · What if A?"
        );
        f.shown(&["lost".into()], NOW + 180.);
        assert_eq!(names(&f.page(p, NOW + 360.)), ["story", "earlier"]);
        assert_eq!(
            f.key(NOW + 360.),
            vec![
                ("dreams".into(), true, false, None),
                ("lost".into(), true, false, None),
                ("story".into(), false, false, None)
            ]
        );
    }
    #[test]
    fn test_content_older_than_stale_moves_once_seen_and_waiting_content_says_it_waits() {
        let (_d, mut f) = setup();
        let mut old = sect("lost", "Wait for the report", Some(NOW - 2701.));
        old["waits"] = json!(true);
        old["brief"] = json!("1 quiet");
        let p = page(vec![sect("story", "S", Some(NOW)), old]);
        assert_eq!(names(&f.page(p.clone(), NOW)), ["story", "lost"]);
        f.shown(&["story".into(), "lost".into()], NOW);
        let got = f.page(p, NOW + 1.);
        assert_eq!(names(&got), ["story", "earlier"]);
        assert_eq!(
            got["sections"][1]["items"][0]["title"],
            "Lost · waits on you · 45m · Wait for the report"
        );
    }
    #[test]
    fn test_a_click_on_an_earlier_row_brings_it_back_under_the_story_until_seen_again() {
        let (_d, mut f) = setup();
        let mut d = sect("dreams", "What if A?", Some(NOW - 2760.));
        d["star"] = json!(true);
        let p = page(vec![
            sect("story", "S", Some(NOW - 2760.)),
            d,
            sect("lost", "L", Some(NOW - 10.)),
        ]);
        f.page(p.clone(), NOW);
        f.shown(&["story".into(), "dreams".into(), "lost".into()], NOW);
        assert_eq!(
            names(&f.page(p.clone(), NOW + 1.)),
            ["story", "lost", "earlier"]
        );
        f.back("dreams", NOW + 2.);
        assert_eq!(
            names(&f.page(p.clone(), NOW + 3.)),
            ["story", "dreams", "lost"]
        );
        f.shown(&["dreams".into()], NOW + 4.);
        assert_eq!(
            names(&f.page(p.clone(), NOW + 183.)),
            ["story", "dreams", "earlier"]
        );
        assert_eq!(names(&f.page(p, NOW + 184.)), ["story", "earlier"]);
    }
    #[test]
    fn test_new_content_is_fresh_again_and_says_new_for_a_minute() {
        let (_d, mut f) = setup();
        let mut story = sect("story", "rev 3 words", Some(NOW - 600.));
        story["right"] = json!("rev 3");
        story["rights"] = json!(["3"]);
        let p = page(vec![story.clone(), sect("lost", "L", Some(NOW - 10.))]);
        f.page(p.clone(), NOW);
        f.shown(&["story".into(), "lost".into()], NOW);
        assert_eq!(names(&f.page(p, NOW + 180.)), ["story", "earlier"]);
        story["items"][0]["title"] = json!("rev 4 words");
        story["right"] = json!("rev 4");
        story["at"] = json!(NOW - 900.);
        let p = page(vec![story]);
        let got = f.page(p.clone(), NOW + 185.);
        assert_eq!(got["sections"][0]["right"], "new · rev 4");
        assert_eq!(got["sections"][0]["rights"], json!(["new · 3", "new"]));
        assert!(f.is_new("story", NOW + 244.));
        assert_eq!(f.page(p, NOW + 245.)["sections"][0]["right"], "rev 4");
    }
    #[test]
    fn test_what_it_knows_is_kept_at_most_every_thirty_seconds_and_a_new_pane_goes_on_from_it() {
        let (d, mut f) = setup();
        let p = page(vec![sect("story", "S", Some(NOW))]);
        f.page(p, NOW);
        f.shown(&["story".into()], NOW);
        f.save(NOW);
        let paths = Paths::new(d.path().into());
        let path = paths.kept_file("seen", "sess-feed").unwrap();
        assert_eq!(read_json(&path)["sections"]["story"]["seen"], NOW);
        f.back("story", NOW + 5.);
        f.save(NOW + 5.);
        assert!(read_json(&path)["sections"]["story"]["back"].is_null());
        f.save(NOW + 30.);
        assert_eq!(read_json(&path)["sections"]["story"]["back"], NOW + 5.);
        let mut again = Feed::new(&paths, "sess-feed");
        let got = again.page(page(vec![sect("story", "S2", Some(NOW))]), NOW + 60.);
        assert!(string(&got["sections"][0]["right"]).starts_with("new"));
    }
    #[test]
    fn test_a_notice_older_than_its_life_is_gone_a_fresh_one_stays_and_the_story_stays_regardless()
    {
        let (_d, mut f) = setup();
        let p = page(vec![
            sect("story", "S", Some(NOW - 15000.)),
            sect("compacted", "274k → 70k", Some(NOW - 301.)),
            sect("remembered", "A note Claude opened", Some(NOW - 100.)),
        ]);
        let got = f.page(p.clone(), NOW);
        assert_eq!(names(&got), ["story", "remembered"]);
        assert!(got["sections"][0]["life"].is_null());
        assert_eq!(num(&got["sections"][1]["life"]), 2. / 3.);
        assert_eq!(names(&f.page(p, NOW + 300.)), ["story"]);
    }
    #[test]
    fn test_an_open_question_and_lost_and_found_wait_on_your_press_however_old() {
        let (_d, mut f) = setup();
        let mut dreams = sect("dreams", "What if A?", Some(NOW - 3000.));
        dreams["star"] = json!(true);
        let got = f.page(
            page(vec![
                sect("story", "S", Some(NOW - 3000.)),
                dreams,
                sect("lost", "L", Some(NOW - 3000.)),
                sect("coach", "C", Some(NOW - 3000.)),
            ]),
            NOW,
        );
        assert_eq!(names(&got), ["story", "dreams", "lost"]);
        assert!(array(&got["sections"]).iter().all(|s| s["life"].is_null()));
    }
    #[test]
    fn test_notice_life_reads_csr_card_life_s_and_the_frame_redraws_as_the_tape_drains() {
        assert_eq!(notice_life_value("80"), 80.);
        for junk in ["0", "-5", "soon", "", "+80"] {
            assert_eq!(notice_life_value(junk), 300.);
        }
        let (_d, mut f) = setup();
        f.page(
            page(vec![
                sect("story", "S", None),
                sect("rules", "R", Some(NOW)),
            ]),
            NOW,
        );
        let life = notice_life();
        assert_eq!(f.life_level("rules", NOW), Some(8));
        assert_eq!(f.life_level("rules", NOW + life / 2.), Some(4));
        assert_eq!(f.life_level("rules", NOW + life), Some(-1));
    }
    #[test]
    fn test_a_held_page_still_lets_a_notice_go_when_its_time_is_up() {
        let held = vec![
            json!({
                "name": "story",
                "born": NOW - 9999.,
            }),
            json!({
                "name": "compacted",
                "born": NOW - 300.,
            }),
            json!({
                "name": "rules",
                "born": NOW - 30.,
            }),
        ];
        let got = living(held, NOW);
        assert_eq!(
            got.iter().map(|s| string(&s["name"])).collect::<Vec<_>>(),
            ["story", "rules"]
        );
        assert_eq!(num(&got[1]["life"]), 0.9);
    }
}

#[cfg(test)]
mod page_parity {
    use super::*;
    #[test]
    fn test_the_page_draws_earlier_as_one_quiet_row_each_on_a_desktop_and_a_phone() {
        let d = tempfile::tempdir().unwrap();
        let mut f = Feed::new(&Paths::new(d.path().into()), "s");
        let p = json!({
            "head": {
                "facts": "",
            },
            "sections": [
                {
                    "name": "story",
                    "title": "Story",
                    "right": "rev 11",
                    "at": 1990,
                    "items": [{
                        "key": "story",
                        "title": "The story",
                        "lines": 1,
                    }],
                },
                {
                    "name": "dreams",
                    "title": "What if…",
                    "right": "4 left",
                    "at": 1990,
                    "star": true,
                    "waits": true,
                    "items": [{
                        "key": "dream:x",
                        "title": "What if A?",
                        "lines": 1,
                    }],
                },
            ],
        });
        f.observe(array(&p["sections"]), 2000.);
        f.shown(&["story".into(), "dreams".into()], 2000.);
        let got = f.page(p, 2180.);
        for phone in [false, true] {
            let lines = super::super::pages::draw_page(
                &got,
                51,
                30,
                &json!({
                    "phone": phone,
                }),
            );
            let text: Vec<_> = lines
                .rows
                .iter()
                .map(|s| super::super::render::text::plain(s).replace("\x1b[K", ""))
                .collect();
            let at = if phone { 2 } else { 6 };
            let head = text
                .iter()
                .position(|s| {
                    s.chars()
                        .skip(at)
                        .collect::<String>()
                        .starts_with("Earlier")
                })
                .unwrap();
            assert!(text[head + 1]
                .chars()
                .skip(at)
                .collect::<String>()
                .starts_with("What if… · waits on you · 3m · "));
            assert!(text[head + 1].chars().count() <= 50);
            assert_eq!(lines.acts[&(head + 1)]["back"], "dreams");
        }
    }
    #[test]
    fn test_a_fresh_notice_draws_a_partly_drained_tape_at_its_headings_end() {
        let section = json!({
            "name": "compacted",
            "title": "Compacted",
            "right": "new · 15:44",
            "life": 0.5,
        });
        let line = super::super::pages::heading(&section, 60, 6);
        let line = super::super::render::text::plain(&line);
        assert!(line.ends_with("new · 15:44 ▄"));
        assert!(line.chars().count() <= 60);
        assert_eq!(fill_levels(0.01, 1), vec![0]);
        assert_eq!(fill_levels(1., 1), vec![8]);
    }
}
