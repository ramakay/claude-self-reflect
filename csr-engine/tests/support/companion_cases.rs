use csr_engine::companion::{brief, coach, lost, paths::Paths};
use serde_json::{json, Value};
use std::fs;

fn pairs(v: &Value) -> Vec<(f64, String)> {
    v.as_array().into_iter().flatten().map(|r| {
        (r[0].as_f64().unwrap(), r[1].as_str().unwrap().to_owned())
    }).collect()
}
fn numbers(v: &mut Value) {
    match v {
        Value::Number(n) => *v = Value::from(n.as_f64().unwrap()),
        Value::Array(a) => a.iter_mut().for_each(numbers),
        Value::Object(o) => o.values_mut().for_each(numbers),
        _ => (),
    }
}
fn check(name: &str) {
    let records: Vec<Value> = serde_json::from_str(include_str!(
        "../fixtures/companion/python-cases.json")).unwrap();
    let mut count = 0;
    for record in records.iter().filter(|r| r["test"] == name) {
        count += 1;
        let input = &record["inputs"];
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let mut actual = match record["mode"].as_str().unwrap() {
            "lost_threads" => {
                let rows = lost::lost_threads(&pairs(&input["summaries"]), &pairs(&input["typed"]));
                let mut value = serde_json::to_value(rows).unwrap();
                for row in value.as_array_mut().unwrap() {
                    row.as_object_mut().unwrap().remove("hidden");
                    row.as_object_mut().unwrap().remove("later");
                }
                value
            }
            "lost_found" => {
                fs::create_dir_all(&paths.keep).unwrap();
                let mut log = String::new();
                if let Some(said) = input["said"].as_object() {
                    for (key, row) in said {
                        log.push_str(&json!({"key":key,"answer":row[0],"at":row[1],
                            "project":input["project"]}).to_string());
                        log.push('\n');
                    }
                }
                fs::write(paths.keep.join("lost-found.jsonl"), log).unwrap();
                let carried: Vec<lost::Thread> = serde_json::from_value(input["carried"].as_array()
                    .map(|a| Value::Array(a.clone())).unwrap_or(json!([]))).unwrap();
                let handoffs: Vec<csr_engine::companion::agents::Handoff> = serde_json::from_value(input["handoffs"].as_array()
                    .map(|a| Value::Array(a.clone())).unwrap_or(json!([]))).unwrap();
                serde_json::to_value(lost::lost_found(&paths, &pairs(&input["summaries"]),
                    &pairs(&input["typed"]), &carried, input["project"].as_str(), &handoffs)).unwrap()
            }
            "terms_looked_up" => {
                let path = d.path().join("s.jsonl");
                fs::write(&path, input["text"].as_str().unwrap()).unwrap();
                serde_json::to_value(coach::terms_looked_up(&path)).unwrap()
            }
            "past_lines" => {
                let path = d.path().join("history.jsonl");
                fs::write(&path, input["text"].as_str().unwrap()).unwrap();
                let (counts, lines) = brief::past_lines(&path, input["session"].as_str().unwrap());
                json!([counts, lines.into_iter().map(|r| (r.at,r.session,r.project,r.text))
                    .collect::<Vec<_>>()])
            }
            "past_rows" => {
                let near = input["near"].as_array().unwrap().iter().map(|r| brief::Past {
                    score:r[0].as_f64().unwrap(), at:r[1].as_f64().unwrap(),
                    session:r[2].as_str().unwrap().into(), project:r[3].as_str().unwrap().into(),
                    text:r[4].as_str().unwrap().into(),
                }).collect::<Vec<_>>();
                serde_json::to_value(brief::past_rows(&near, input["now"].as_f64().unwrap())).unwrap()
            }
            "dream_rows" => serde_json::to_value(brief::dream_rows(
                input["dreams"].as_array().unwrap())).unwrap(),
            mode => panic!("unhandled mode {mode}"),
        };
        let mut expected = record["expected"].clone();
        numbers(&mut actual); numbers(&mut expected);
        assert_eq!(actual, expected, "{name}, {}", record["mode"]);
    }
    assert!(count > 0, "no oracle calls captured for {name}");
}
