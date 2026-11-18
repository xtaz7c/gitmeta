use gitmeta::event::{data, strs, Author, Event};
use gitmeta::state::{replay, ItemKind, State};
use gitmeta::sync::sync;
use gitmeta::{EventStore, GitStore, Store};
use serde_json::json;
use std::process::ExitCode;

const USO: &str = "uso: gitmeta [--dir PASTA] <comando> ...
  (sem --dir: grava na ref refs/meta/issues do repositório Git atual)
  sync <remote>
  list
  show <id>
  issue-open <título> [corpo]
  comment <id> <texto>
  label <id> <rótulo>
  close <id> [done|wontfix|duplicate]
  reopen <id>
  pr-open <título> <base> <head> <sha>
  pr-update <id> <sha>
  review <id> <approved|changes_requested|commented> <sha>
  checks <id> <sha> <pass|fail>
  merge <id> <sha-do-merge>
autor via GITMETA_NAME e GITMETA_EMAIL";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erro: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut dir: Option<String> = None;
    if args.first().map(String::as_str) == Some("--dir") {
        if args.len() < 2 {
            return Err(USO.into());
        }
        dir = Some(args[1].clone());
        args.drain(0..2);
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    if matches!(a.as_slice(), ["--version"] | ["version"]) {
        println!("gitmeta {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if let ["sync", remote] = a.as_slice() {
        let g = GitStore::discover(".").map_err(|_| "sync precisa estar dentro de um repositório Git")?;
        println!("{}", sync(&g, remote)?);
        return Ok(());
    }
    let store: Box<dyn EventStore> = match dir {
        Some(d) => Box::new(Store::open(d)?),
        None => Box::new(GitStore::discover(".").map_err(|_| "não estou num repositório Git (use --dir PASTA)")?),
    };
    let store = store.as_ref();
    let author = Author {
        name: std::env::var("GITMETA_NAME").unwrap_or_else(|_| "anon".into()),
        email: std::env::var("GITMETA_EMAIL").unwrap_or_else(|_| "anon@local".into()),
    };

    match a.as_slice() {
        ["list"] => {
            for t in store.targets()? {
                if let Some(i) = replay(&store.load(&t)?.events) {
                    let k = if i.kind == ItemKind::Pr { "PR   " } else { "ISSUE" };
                    println!("{t}  {k}  {:?}  {}", i.state, i.title);
                }
            }
        }
        ["show", id] => {
            let l = store.load(id)?;
            let i = replay(&l.events).ok_or("item não encontrado")?;
            println!("{} [{:?}] {}\n{}", i.id, i.state, i.title, i.body);
            println!("autor: {}  comentários: {}  rótulos: {:?}", i.author, i.comments, i.labels);
            if i.kind == ItemKind::Pr {
                println!("{} -> {}  head: {}", i.head, i.base, i.head_sha);
                let b = i.merge_blockers();
                if b.is_empty() {
                    println!("merge: liberado");
                } else {
                    println!("merge bloqueado:");
                    for x in b {
                        println!("  - {x}");
                    }
                }
            }
            if !l.skipped.is_empty() {
                eprintln!("aviso: {} arquivo(s) ignorado(s): {:?}", l.skipped.len(), l.skipped);
            }
        }
        ["issue-open", title, rest @ ..] => {
            let ev = Event::new("issue.opened", None, author, data(&[("title", json!(title)), ("body", json!(rest.first().unwrap_or(&""))), ("labels", strs(&[]))]));
            store.append(&ev)?;
            println!("{}", ev.id);
        }
        ["comment", id, text] => {
            let kind = kind_of(store, id)?;
            let t = if kind == ItemKind::Pr { "pr.commented" } else { "issue.commented" };
            store.append(&Event::new(t, Some(id), author, data(&[("body", json!(text))])))?;
        }
        ["label", id, l] => {
            store.append(&Event::new("issue.labeled", Some(id), author, data(&[("add", strs(&[l])), ("remove", strs(&[]))])))?;
        }
        ["close", id, rest @ ..] => {
            let t = if kind_of(store, id)? == ItemKind::Pr { "pr.closed" } else { "issue.closed" };
            store.append(&Event::new(t, Some(id), author, data(&[("reason", json!(rest.first().unwrap_or(&"done")))])))?;
        }
        ["reopen", id] => {
            let t = if kind_of(store, id)? == ItemKind::Pr { "pr.reopened" } else { "issue.reopened" };
            store.append(&Event::new(t, Some(id), author, json!({})))?;
        }
        ["pr-open", title, base, head, sha] => {
            let ev = Event::new("pr.opened", None, author, data(&[("title", json!(title)), ("body", json!("")), ("base", json!(base)), ("head", json!(head)), ("head_sha", json!(sha))]));
            store.append(&ev)?;
            println!("{}", ev.id);
        }
        ["pr-update", id, sha] => {
            store.append(&Event::new("pr.updated", Some(id), author, data(&[("head_sha", json!(sha))])))?;
        }
        ["review", id, st, sha] => {
            store.append(&Event::new("pr.reviewed", Some(id), author, data(&[("state", json!(st)), ("body", json!("")), ("sha", json!(sha))])))?;
        }
        ["checks", id, sha, res] => {
            let status = if *res == "pass" { "pass" } else { "fail" };
            store.append(&Event::new("pr.checks", Some(id), author, data(&[("sha", json!(sha)), ("results", json!([{ "name": "workflow", "status": status }]))])))?;
        }
        ["merge", id, msha] => {
            let i = replay(&store.load(id)?.events).ok_or("item não encontrado")?;
            let b = i.merge_blockers();
            if !b.is_empty() {
                return Err(format!("merge bloqueado: {}", b.join("; ")).into());
            }
            store.append(&Event::new("pr.merged", Some(id), author, data(&[("merge_sha", json!(msha)), ("strategy", json!("merge"))])))?;
            let _ = State::Merged;
        }
        _ => return Err(USO.into()),
    }
    Ok(())
}

fn kind_of(store: &dyn EventStore, id: &str) -> Result<ItemKind, Box<dyn std::error::Error>> {
    Ok(replay(&store.load(id)?.events).ok_or("item não encontrado")?.kind)
}
