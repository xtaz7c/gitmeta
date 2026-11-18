use gitmeta::event::{data, Author, Event};
use gitmeta::{state::replay, EventStore, GitStore, Store};
use serde_json::json;
use std::time::Instant;

fn rodar(nome: &str, s: &dyn EventStore, n: u32) {
    let a = Author { name: "b".into(), email: "b@x".into() };
    let open = Event::new("issue.opened", None, a.clone(), data(&[("title", json!("bench"))]));
    s.append(&open).unwrap();
    let t = Instant::now();
    for i in 0..n {
        s.append(&Event::new("issue.commented", Some(&open.id), a.clone(), data(&[("body", json!(format!("c{i}")))]))).unwrap();
    }
    let w = t.elapsed();
    let t = Instant::now();
    let l = s.load(&open.id).unwrap();
    let it = replay(&l.events).unwrap();
    println!("[{nome}] escrita: {n} eventos em {w:?} ({:?}/evento) | leitura+replay: {} eventos em {:?} (comentários={})", w / n, l.events.len(), t.elapsed(), it.comments);
}

fn main() {
    let base = std::env::temp_dir().join(format!("gitmeta-bench-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&base).unwrap();
    rodar("pasta", &Store::open(base.join("pasta")).unwrap(), 2000);
    git2::Repository::init(base.join("repo")).unwrap();
    rodar("ref git", &GitStore::open(base.join("repo")).unwrap(), 2000);
    std::fs::remove_dir_all(base).ok();
}
