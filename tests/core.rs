use gitmeta::event::{data, strs, Author, Event};
use gitmeta::state::{replay, State};
use gitmeta::{Store, StoreError};
use serde_json::json;

fn tmp() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gitmeta-test-{}", ulid::Ulid::new()))
}
fn me(email: &str) -> Author {
    Author { name: "Teste".into(), email: email.into() }
}
fn pr(store: &Store, sha: &str) -> String {
    let ev = Event::new("pr.opened", None, me("a@x"), data(&[("title", json!("PR")), ("body", json!("")), ("base", json!("main")), ("head", json!("f")), ("head_sha", json!(sha))]));
    store.append(&ev).unwrap();
    ev.id
}
fn act(store: &Store, id: &str, t: &str, who: &str, d: serde_json::Value) {
    store.append(&Event::new(t, Some(id), me(who), d)).unwrap();
}

#[test]
fn issue_replay_completo() {
    let s = Store::open(tmp()).unwrap();
    let ev = Event::new("issue.opened", None, me("a@x"), data(&[("title", json!("bug")), ("body", json!("")), ("labels", strs(&["ui"]))]));
    s.append(&ev).unwrap();
    act(&s, &ev.id, "issue.labeled", "a@x", data(&[("add", strs(&["p1"])), ("remove", strs(&["ui"]))]));
    act(&s, &ev.id, "issue.commented", "b@x", data(&[("body", json!("oi"))]));
    act(&s, &ev.id, "issue.closed", "a@x", data(&[("reason", json!("done"))]));
    let i = replay(&s.load(&ev.id).unwrap().events).unwrap();
    assert_eq!(i.state, State::Closed);
    assert_eq!(i.comments, 1);
    assert_eq!(i.labels.iter().collect::<Vec<_>>(), vec!["p1"]);
}

#[test]
fn evento_e_imutavel() {
    let s = Store::open(tmp()).unwrap();
    let ev = Event::new("issue.opened", None, me("a@x"), data(&[("title", json!("x"))]));
    s.append(&ev).unwrap();
    assert!(matches!(s.append(&ev), Err(StoreError::Exists)));
}

#[test]
fn bloqueia_path_traversal_e_tipo_desconhecido() {
    let s = Store::open(tmp()).unwrap();
    let mut ev = Event::new("issue.opened", None, me("a@x"), data(&[("title", json!("x"))]));
    ev.target = "../../etc".into();
    assert!(matches!(s.append(&ev), Err(StoreError::Invalid(_))));
    assert!(s.load("../../etc").is_err());
    let ev2 = Event::new("issue.hackeado", None, me("a@x"), json!({}));
    assert!(matches!(s.append(&ev2), Err(StoreError::Invalid(_))));
}

#[test]
fn rejeita_titulo_gigante() {
    let s = Store::open(tmp()).unwrap();
    let ev = Event::new("issue.opened", None, me("a@x"), data(&[("title", json!("a".repeat(5000)))]));
    assert!(matches!(s.append(&ev), Err(StoreError::Invalid(_))));
}

#[test]
fn arquivo_corrompido_e_ignorado_sem_quebrar() {
    let root = tmp();
    let s = Store::open(&root).unwrap();
    let ev = Event::new("issue.opened", None, me("a@x"), data(&[("title", json!("ok"))]));
    s.append(&ev).unwrap();
    let bad = root.join(&ev.id).join(format!("{}.json", ulid::Ulid::new()));
    std::fs::write(bad, b"{lixo").unwrap();
    let l = s.load(&ev.id).unwrap();
    assert_eq!(l.events.len(), 1);
    assert_eq!(l.skipped.len(), 1);
}

#[test]
fn merge_exige_aprovacao_no_sha_atual_e_checks_verdes() {
    let s = Store::open(tmp()).unwrap();
    let id = pr(&s, "aaa");
    let blk = |s: &Store| replay(&s.load(&id).unwrap().events).unwrap().merge_blockers();
    assert!(!blk(&s).is_empty());

    act(&s, &id, "pr.reviewed", "rev@x", data(&[("state", json!("approved")), ("body", json!("")), ("sha", json!("aaa"))]));
    act(&s, &id, "pr.checks", "bot@x", data(&[("sha", json!("aaa")), ("results", json!([{ "name": "lint", "status": "pass" }]))]));
    assert!(blk(&s).is_empty(), "deveria liberar: {:?}", blk(&s));

    // Push novo: aprovação e checks antigos ficam desatualizados.
    act(&s, &id, "pr.updated", "a@x", data(&[("head_sha", json!("bbb"))]));
    assert_eq!(blk(&s).len(), 2);

    // Merge forçado sem regras não vale no replay.
    act(&s, &id, "pr.merged", "a@x", data(&[("merge_sha", json!("m1")), ("strategy", json!("merge"))]));
    assert_eq!(replay(&s.load(&id).unwrap().events).unwrap().state, State::Open);
}

#[test]
fn changes_requested_bloqueia() {
    let s = Store::open(tmp()).unwrap();
    let id = pr(&s, "aaa");
    act(&s, &id, "pr.reviewed", "r1@x", data(&[("state", json!("approved")), ("body", json!("")), ("sha", json!("aaa"))]));
    act(&s, &id, "pr.reviewed", "r2@x", data(&[("state", json!("changes_requested")), ("body", json!("")), ("sha", json!("aaa"))]));
    act(&s, &id, "pr.checks", "bot@x", data(&[("sha", json!("aaa")), ("results", json!([{ "name": "t", "status": "pass" }]))]));
    let b = replay(&s.load(&id).unwrap().events).unwrap().merge_blockers();
    assert_eq!(b.len(), 1);
}
