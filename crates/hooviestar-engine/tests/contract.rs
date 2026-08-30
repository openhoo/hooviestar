use hooviestar_engine::{EngineCommand, EngineEvent, ProjectV1};
use serde_json::Value;

#[test]
fn shared_project_fixture_round_trips() {
    let raw = load_project_fixture();
    let project: ProjectV1 = serde_json::from_value(raw.clone()).unwrap();
    project.validate().unwrap();
    let encoded = serde_json::to_value(project).unwrap();
    assert_canon_equal("contracts/project-v1.json", &raw, &encoded);
    assert_eq!(encoded["sources"][0]["type"], "window");
    assert_eq!(encoded["sources"][5]["type"], "application_audio");
}

#[test]
fn every_command_tag_round_trips() {
    let fixtures = load_command_fixtures();
    assert_eq!(fixtures.len(), 19, "one wire sample per command tag");
    assert_unique_type_tags(&fixtures);
    assert_round_trip::<EngineCommand>("commands", &fixtures);
}

#[test]
fn every_event_tag_round_trips() {
    let fixtures = load_event_fixtures();
    assert_eq!(fixtures.len(), 11, "one wire sample per event tag");
    assert_unique_type_tags(&fixtures);
    assert_round_trip::<EngineEvent>("events", &fixtures);
}

fn load_project_fixture() -> Value {
    serde_json::from_str(include_str!("../../../contracts/project-v1.json")).unwrap()
}

fn load_command_fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("../../../contracts/commands-v1.json")).unwrap()
}

fn load_event_fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("../../../contracts/events-v1.json")).unwrap()
}

/// Pins each fixture to a distinct `type` tag so every wire file covers each
/// variant exactly once.
fn assert_unique_type_tags(fixtures: &[Value]) {
    let mut tags: Vec<&str> = fixtures
        .iter()
        .map(|fixture| fixture["type"].as_str().expect("sample carries a type tag"))
        .collect();
    tags.sort_unstable();
    let total = tags.len();
    tags.dedup();
    assert_eq!(
        tags.len(),
        total,
        "each sample must pin a distinct type tag"
    );
}

fn assert_round_trip<T>(kind: &str, fixtures: &[Value])
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    for (index, fixture) in fixtures.iter().enumerate() {
        let name = format!("{kind}[{index}] {}", fixture["type"]);
        let decoded: T = serde_json::from_value(fixture.clone())
            .unwrap_or_else(|error| panic!("fixture {name} failed to decode: {error}"));
        let encoded = serde_json::to_value(decoded).unwrap();
        assert_canon_equal(&name, fixture, &encoded);
    }
}

fn assert_canon_equal(name: &str, original: &Value, encoded: &Value) {
    assert!(
        canon(original) == canon(encoded),
        "fixture {name}: re-encoded JSON diverged\n  original: {original}\n  re-encoded: {encoded}"
    );
}

/// Canonicalizes every number to f64 so integer-vs-float reserialization
/// (`1` vs `1.0`) does not count as drift, while dropped or renamed fields do.
fn canon(value: &Value) -> Value {
    match value {
        Value::Number(number) => {
            let float = number.as_f64().expect("JSON numbers fit into f64");
            Value::Number(serde_json::Number::from_f64(float).expect("finite f64"))
        }
        Value::Array(items) => Value::Array(items.iter().map(canon).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(field, item)| (field.clone(), canon(item)))
                .collect(),
        ),
        other => other.clone(),
    }
}
