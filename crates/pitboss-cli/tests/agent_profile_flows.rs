//! End-to-end flows exercising `[[agent_profile]]` resolution and spawn
//! composition without booting an actual claude subprocess. Verifies that
//! the prelude / env / model / tools precedence cascade is observable in
//! the resolved manifest the dispatcher hands to argv builders.

use std::collections::HashMap;

use pitboss_cli::manifest::resolve::{compose_prompt, resolve};
use pitboss_cli::manifest::schema::{AgentProfile, Manifest};

fn minimal_manifest_with_lead(lead_prompt: &str, lead_profile: Option<&str>) -> String {
    let profile_line = match lead_profile {
        Some(id) => format!("agent_profile = \"{id}\"\n"),
        None => String::new(),
    };
    format!(
        r#"
[lead]
id = "test-lead"
directory = "/tmp"
prompt = "{lead_prompt}"
{profile_line}
"#
    )
}

#[test]
fn builtin_lead_profile_is_prepended_to_lead_prompt() {
    let toml_src = minimal_manifest_with_lead("plan the test work", Some("pitboss/lead-opus"));
    let m: Manifest = toml::from_str(&toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let lead = r.lead.expect("lead present");

    // The composed prompt should contain BOTH the built-in lead body and
    // the operator's prompt, joined by the canonical separator.
    assert!(
        lead.prompt.contains("Pitboss **lead**"),
        "built-in prelude missing from composed prompt:\n{}",
        lead.prompt
    );
    assert!(
        lead.prompt.contains("--- TASK ---"),
        "task separator missing:\n{}",
        lead.prompt
    );
    assert!(
        lead.prompt.contains("plan the test work"),
        "operator prompt missing from composed prompt:\n{}",
        lead.prompt
    );

    // Operator prompt must appear AFTER the separator, not before.
    let sep = lead.prompt.find("--- TASK ---").unwrap();
    let op = lead.prompt.find("plan the test work").unwrap();
    assert!(
        op > sep,
        "operator prompt should follow the separator; prompt:\n{}",
        lead.prompt
    );
}

#[test]
fn unprofiled_lead_prompt_is_unchanged() {
    let toml_src = minimal_manifest_with_lead("plain prompt", None);
    let m: Manifest = toml::from_str(&toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let lead = r.lead.expect("lead present");
    assert_eq!(lead.prompt, "plain prompt");
    assert!(!lead.prompt.contains("--- TASK ---"));
}

#[test]
fn lead_profile_provides_env_default_without_clobbering_operator_env() {
    // worker-haiku built-in carries PITBOSS_ACTOR_ROLE=worker; if we
    // use lead-opus, we'll see PITBOSS_ACTOR_ROLE=lead in the env even
    // though no manifest field set it.
    let toml_src = minimal_manifest_with_lead("hi", Some("pitboss/lead-opus"));
    let m: Manifest = toml::from_str(&toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let lead = r.lead.expect("lead present");
    assert_eq!(
        lead.env.get("PITBOSS_ACTOR_ROLE").map(String::as_str),
        Some("lead"),
        "profile env should populate role env:\n{:#?}",
        lead.env
    );
}

#[test]
fn manifest_lead_env_overrides_profile_env() {
    let toml_src = r#"
[lead]
id = "x"
directory = "/tmp"
prompt = "hi"
agent_profile = "pitboss/lead-opus"
env = { PITBOSS_ACTOR_ROLE = "custom-role" }
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let lead = r.lead.expect("lead present");
    assert_eq!(
        lead.env.get("PITBOSS_ACTOR_ROLE").map(String::as_str),
        Some("custom-role"),
        "operator lead env must win over profile default"
    );
}

#[test]
fn manifest_lead_model_overrides_profile_model() {
    let toml_src = r#"
[lead]
id = "x"
directory = "/tmp"
prompt = "hi"
agent_profile = "pitboss/lead-opus"
model = "claude-haiku-4-5"
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let lead = r.lead.expect("lead present");
    // Operator wins.
    assert_eq!(lead.model, "claude-haiku-4-5");
}

#[test]
fn profile_model_default_takes_effect_when_lead_omits_it() {
    let toml_src = r#"
[lead]
id = "x"
directory = "/tmp"
prompt = "hi"
agent_profile = "pitboss/lead-opus"
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let lead = r.lead.expect("lead present");
    // Default is Sonnet absent a profile; lead-opus profile contributes
    // the Opus default.
    assert_eq!(lead.model, "claude-opus-4-7");
}

#[test]
fn dangling_lead_profile_reference_fails_resolve() {
    let toml_src = r#"
[lead]
id = "x"
directory = "/tmp"
prompt = "hi"
agent_profile = "this/does-not-exist"
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let err = resolve(m, None).unwrap_err().to_string();
    assert!(
        err.contains("this/does-not-exist"),
        "error should name the bad id: {err}"
    );
    assert!(err.contains("[lead]"), "error should name the surface");
}

#[test]
fn manifest_profile_shadows_builtin_id() {
    let toml_src = r#"
[lead]
id = "x"
directory = "/tmp"
prompt = "go"
agent_profile = "pitboss/lead-opus"

[[agent_profile]]
id = "pitboss/lead-opus"
system_prompt = "SHADOWED LEAD"
model = "claude-sonnet-4-6"
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let lead = r.lead.expect("lead present");
    assert!(
        lead.prompt.starts_with("SHADOWED LEAD"),
        "manifest shadow should win:\n{}",
        lead.prompt
    );
    assert_eq!(lead.model, "claude-sonnet-4-6");
}

#[test]
fn task_profile_is_prepended_at_resolve_time() {
    let toml_src = r#"
[[agent_profile]]
id = "custom/role"
system_prompt = "PROFILE BODY"
model = "claude-haiku-4-5"

[[task]]
id = "t1"
directory = "/tmp"
prompt = "do work"
agent_profile = "custom/role"
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    assert_eq!(r.tasks.len(), 1);
    let t = &r.tasks[0];
    assert!(t.prompt.contains("PROFILE BODY"), "{}", t.prompt);
    assert!(t.prompt.contains("--- TASK ---"), "{}", t.prompt);
    assert!(t.prompt.contains("do work"), "{}", t.prompt);
    assert_eq!(t.model, "claude-haiku-4-5");
}

#[test]
fn duplicate_manifest_profile_id_rejected_at_resolve() {
    let toml_src = r#"
[[agent_profile]]
id = "x/y"
system_prompt = "first"

[[agent_profile]]
id = "x/y"
system_prompt = "second"

[[task]]
id = "t"
directory = "/tmp"
prompt = "go"
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let err = resolve(m, None).unwrap_err().to_string();
    assert!(err.contains("duplicate"), "{err}");
    assert!(err.contains("x/y"), "{err}");
}

#[test]
fn agent_profiles_catalogue_carries_builtins_and_manifest_entries() {
    let toml_src = r#"
[[agent_profile]]
id = "custom/x"
system_prompt = "x"

[[task]]
id = "t"
directory = "/tmp"
prompt = "go"
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    // Three built-ins + one manifest entry.
    assert!(r.agent_profiles.contains_key("pitboss/lead-opus"));
    assert!(r.agent_profiles.contains_key("pitboss/sublead-sonnet"));
    assert!(r.agent_profiles.contains_key("pitboss/worker-haiku"));
    assert!(r.agent_profiles.contains_key("custom/x"));
    assert_eq!(r.agent_profiles.len(), 4);
}

#[test]
fn compose_prompt_helper_passes_through_when_no_profile() {
    let out = compose_prompt(None, "raw operator content");
    assert_eq!(out, "raw operator content");
}

#[test]
fn compose_prompt_helper_passes_through_when_profile_has_empty_body() {
    let p = AgentProfile {
        id: "empty/x".into(),
        system_prompt: String::new(),
        model: None,
        env: HashMap::new(),
        tools: None,
    };
    let out = compose_prompt(Some(&p), "raw");
    assert_eq!(out, "raw");
}

#[test]
fn validate_rejects_pitboss_namespace_squatting() {
    use pitboss_cli::manifest::validate::validate;
    let toml_src = r#"
[[agent_profile]]
id = "pitboss/not-real"
system_prompt = "x"

[[task]]
id = "t"
directory = "/tmp"
prompt = "go"
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let err = validate(&r).unwrap_err().to_string();
    assert!(err.contains("pitboss/"), "{err}");
    assert!(err.contains("reserved"), "{err}");
}

#[test]
fn resolved_prompt_round_trips_with_exactly_one_prelude() {
    // Regression guard for the "no double-prepend on resume" invariant.
    // The resume path reads ResolvedManifest from resolved.json; if a
    // future refactor were to ALSO re-apply compose_prompt at resume
    // time, we'd end up with two copies of the prelude and two
    // `--- TASK ---` separators. Pin the contract: a resolve →
    // serialize → deserialize round trip on a profile-using manifest
    // must show exactly ONE prelude and ONE separator.
    let toml_src = minimal_manifest_with_lead("task body", Some("pitboss/lead-opus"));
    let m: Manifest = toml::from_str(&toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let prompt_before = r.lead.as_ref().expect("lead").prompt.clone();
    assert_eq!(prompt_before.matches("--- TASK ---").count(), 1);
    assert_eq!(prompt_before.matches("Pitboss **lead**").count(), 1);

    let json = serde_json::to_string(&r).expect("serialize");
    let r2: pitboss_cli::manifest::resolve::ResolvedManifest =
        serde_json::from_str(&json).expect("deserialize");
    let prompt_after = r2.lead.expect("lead present after round-trip").prompt;
    assert_eq!(prompt_before, prompt_after);
    assert_eq!(
        prompt_after.matches("--- TASK ---").count(),
        1,
        "resume-shaped round trip must not duplicate the TASK separator"
    );
    assert_eq!(
        prompt_after.matches("Pitboss **lead**").count(),
        1,
        "resume-shaped round trip must not duplicate the prelude"
    );
}

#[test]
fn defaults_model_beats_profile_model_at_resolve() {
    // Precedence pin: [defaults].model fills the model slot BEFORE the
    // profile's default. An operator with a [defaults] block and a
    // profile reference gets [defaults].model, not the profile's.
    let toml_src = r#"
[defaults]
model = "claude-sonnet-4-6"

[lead]
id = "x"
directory = "/tmp"
prompt = "go"
agent_profile = "pitboss/lead-opus"
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let lead = r.lead.expect("lead present");
    assert_eq!(
        lead.model, "claude-sonnet-4-6",
        "[defaults].model must beat profile.model"
    );
}

#[test]
fn lead_tools_override_profile_tools_at_resolve() {
    // [lead].tools (explicit) wins over profile.tools (default).
    let toml_src = r#"
[lead]
id = "x"
directory = "/tmp"
prompt = "go"
agent_profile = "pitboss/worker-haiku"
tools = ["Bash"]
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let lead = r.lead.expect("lead present");
    assert_eq!(lead.tools, vec!["Bash".to_string()]);
}

#[test]
fn defaults_tools_beat_profile_tools_when_lead_omits() {
    let toml_src = r#"
[defaults]
tools = ["Read", "Write"]

[lead]
id = "x"
directory = "/tmp"
prompt = "go"
agent_profile = "pitboss/worker-haiku"
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let lead = r.lead.expect("lead present");
    assert_eq!(lead.tools, vec!["Read".to_string(), "Write".to_string()]);
}

#[test]
fn validate_rejects_unknown_worker_type_profile_ref() {
    use pitboss_cli::manifest::validate::validate_skip_dir_check;
    let toml_src = r#"
[run]
worktree_cleanup = "never"

[[worker_type]]
id = "extraction"
tools = ["Read"]
agent_profile = "does/not-exist"

[lead]
id = "x"
directory = "/tmp"
prompt = "go"
max_workers = 1
budget_usd = 1.0
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let r = resolve(m, None).expect("resolve");
    let err = validate_skip_dir_check(&r).unwrap_err().to_string();
    assert!(err.contains("does/not-exist"), "{err}");
    assert!(err.contains("worker_type"), "{err}");
}
