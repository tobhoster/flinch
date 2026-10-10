use super::*;
use rstest::rstest;
use std::collections::HashMap;

fn default(app: App) -> Connection {
    Connection {
        app,
        name: String::new(),
        base: format!("http://{}", app.label()),
        key: "k".into(),
        archive_root: String::new(),
        compact_profile: String::new(),
        public_url: String::new(),
    }
}

fn config(app: App, name: &str, key_env: &str) -> InstanceConfig {
    InstanceConfig {
        app,
        name: name.into(),
        url: "http://radarr-4k:7878/".into(),
        key_env: key_env.into(),
        archive_root: String::new(),
        compact_profile: String::new(),
        public_url: String::new(),
    }
}

fn resolve_with(configured: &[InstanceConfig], env: &[(&str, &str)]) -> (Vec<Connection>, Vec<String>) {
    let env: HashMap<String, String> = env.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect();
    let names: Vec<String> = env.keys().cloned().collect();
    resolve([default(App::Radarr), default(App::Sonarr)], configured, |name| env.get(name).cloned(), names)
}

#[test]
fn settings_and_numbered_env_add_instances_after_the_defaults() {
    let (connections, problems) = resolve_with(
        &[config(App::Radarr, "4k", "RADARR_4K_KEY")],
        &[("RADARR_4K_KEY", "secret"), ("SONARR_2_URL", "http://anime:8989/"), ("SONARR_2_API_KEY", "s2"), ("SONARR_2_NAME", "anime")],
    );
    assert!(problems.is_empty(), "{problems:?}");
    let keys: Vec<String> = connections.iter().map(Connection::key).collect();
    assert_eq!(keys, ["radarr", "sonarr", "radarr@4k", "sonarr@anime"]);
    assert_eq!((connections[2].base.as_str(), connections[2].key.as_str()), ("http://radarr-4k:7878", "secret"));
    assert_eq!((connections[3].base.as_str(), connections[3].key.as_str()), ("http://anime:8989", "s2"));
}

#[test]
fn a_numbered_instance_without_a_name_is_named_by_its_number() {
    let (connections, _) = resolve_with(&[], &[("RADARR_3_URL", "http://r3"), ("RADARR_3_API_KEY", "k3")]);
    assert_eq!(connections.last().map(Connection::key).as_deref(), Some("radarr@3"));
}

#[rstest]
#[case::missing_key(&[config(App::Radarr, "4k", "RADARR_4K_KEY")], &[], "RADARR_4K_KEY is unset")]
#[case::half_numbered(&[], &[("RADARR_2_URL", "http://r2")], "_URL and _API_KEY are both needed")]
#[case::bad_numbered_name(&[], &[("RADARR_2_URL", "http://r2"), ("RADARR_2_API_KEY", "k"), ("RADARR_2_NAME", "4K-HD")], "is not 1-24")]
#[case::repeated(
    &[config(App::Radarr, "2", "K")],
    &[("K", "k"), ("RADARR_2_URL", "http://r2"), ("RADARR_2_API_KEY", "k")],
    "a second instance with this name is ignored"
)]
fn a_broken_extra_is_left_out_and_said(#[case] configured: &[InstanceConfig], #[case] env: &[(&str, &str)], #[case] said: &str) {
    let (connections, problems) = resolve_with(configured, env);
    assert!(problems.iter().any(|problem| problem.contains(said)), "{problems:?}");
    assert!(connections.len() <= 3, "the defaults always stay: {connections:?}");
    assert_eq!(connections[0].key(), "radarr");
}

#[rstest]
#[case::fine(config(App::Radarr, "4k", "RADARR_4K_KEY"), true)]
#[case::bad_name(config(App::Radarr, "4K", "RADARR_4K_KEY"), false)]
#[case::a_key_not_an_env_name(config(App::Radarr, "4k", "0123abcd-secret"), false)]
#[case::no_scheme(InstanceConfig { url: "radarr:7878".into(), ..config(App::Radarr, "4k", "K") }, false)]
fn settings_validate_each_instance(#[case] instance: InstanceConfig, #[case] valid: bool) {
    assert_eq!(validate(&[instance]).is_ok(), valid);
}

#[test]
fn one_name_per_app_but_the_same_name_in_each_app() {
    let shared = [config(App::Radarr, "uhd", "A"), config(App::Sonarr, "uhd", "B")];
    assert!(validate(&shared).is_ok());
    assert!(validate(&[config(App::Radarr, "uhd", "A"), config(App::Radarr, "uhd", "B")]).is_err());
}
