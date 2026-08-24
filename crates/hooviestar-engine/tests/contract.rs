use hooviestar_engine::{EngineCommand, EngineEvent, ProjectV1};
use serde_json::{Value, json};

#[test]
fn shared_project_fixture_round_trips() {
    let raw = include_str!("../../../contracts/project-v1.json");
    let project: ProjectV1 = serde_json::from_str(raw).unwrap();
    project.validate().unwrap();
    let encoded = serde_json::to_value(project).unwrap();
    assert_eq!(encoded["sources"][0]["type"], "window");
    assert_eq!(encoded["sources"][5]["type"], "application_audio");
}

#[test]
fn every_command_tag_round_trips() {
    let id = "00000000-0000-4000-8000-000000000001";
    let transform = json!({"x":0,"y":0,"width":1280,"height":720,"rotationDegrees":0,"cropTop":0,"cropRight":0,"cropBottom":0,"cropLeft":0,"opacity":1});
    let source = json!({"type":"image","id":id,"name":"Bild","path":"C:\\image.png"});
    let output = json!({"width":1280,"height":720,"fps":30,"background":"#101418"});
    let fixtures = vec![
        json!({"type":"add_source","source":source}),
        json!({"type":"remove_source","sourceId":id}),
        json!({"type":"update_source","source":source}),
        json!({"type":"add_scene","sceneId":id,"name":"Szene"}),
        json!({"type":"remove_scene","sceneId":id}),
        json!({"type":"rename_scene","sceneId":id,"name":"Neu"}),
        json!({"type":"set_active_scene","sceneId":id}),
        json!({"type":"set_scene_hotkey","sceneId":id,"hotkey":"Ctrl+Alt+1"}),
        json!({"type":"add_scene_item","sceneId":id,"itemId":id,"sourceId":id,"transform":transform}),
        json!({"type":"remove_scene_item","sceneId":id,"itemId":id}),
        json!({"type":"set_item_visible","sceneId":id,"itemId":id,"visible":true}),
        json!({"type":"set_item_locked","sceneId":id,"itemId":id,"locked":true}),
        json!({"type":"reorder_scene_item","sceneId":id,"itemId":id,"index":0}),
        json!({"type":"set_transform","sceneId":id,"itemId":id,"transform":transform}),
        json!({"type":"set_output_config","output":output}),
        json!({"type":"set_media_playing","sourceId":id,"playing":true}),
        json!({"type":"media_seek","sourceId":id,"positionSeconds":1.25}),
        json!({"type":"set_audio_volume","sourceId":id,"volume":0.5}),
        json!({"type":"set_audio_muted","sourceId":id,"muted":true}),
    ];
    assert_round_trip_tags::<EngineCommand>(&fixtures);
}

#[test]
fn every_event_tag_round_trips() {
    let id = "00000000-0000-4000-8000-000000000001";
    let project: Value =
        serde_json::from_str(include_str!("../../../contracts/project-v1.json")).unwrap();
    let fixtures = vec![
        json!({"type":"snapshot","project":project}),
        json!({"type":"source_available","sourceId":id}),
        json!({"type":"source_unavailable","sourceId":id,"reason":"offline"}),
        json!({"type":"levels","entries":[{"sourceId":id,"peak":0.5,"rms":0.25}]}),
        json!({"type":"hotkey_error","sceneId":id,"message":"conflict"}),
        json!({"type":"device_recovery","phase":"started","detail":null}),
        json!({"type":"media_state","sourceId":id,"state":{"playing":true,"positionSeconds":1,"durationSeconds":2}}),
        json!({"type":"unsupported_media","sourceId":id,"reason":"codec"}),
        json!({"type":"audio_warning","kind":"underrun","message":"empty"}),
        json!({"type":"engine_error","message":"failed"}),
    ];
    assert_round_trip_tags::<EngineEvent>(&fixtures);
}

fn assert_round_trip_tags<T>(fixtures: &[Value])
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    for fixture in fixtures {
        let decoded: T = serde_json::from_value(fixture.clone()).unwrap();
        let encoded = serde_json::to_value(decoded).unwrap();
        assert_eq!(encoded["type"], fixture["type"]);
    }
}
