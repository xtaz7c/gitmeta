use gitmeta::event::{data, strs, Author, Event};
use gitmeta::gitstore::{GitStore, MergeOutcome, REF};
use gitmeta::state::{replay, State};
use gitmeta::sync::sync;
use gitmeta::{EventStore, StoreError};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;

fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("gitmeta-{tag}-{}", ulid::Ulid::new()))
}
fn me(email: &str) -> Author {
    Author { name: "Teste".into(), email: email.into() }
}
fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success();
    assert!(ok, "git {args:?} falhou");
}
fn novo_repo() -> (PathBuf, GitStore) {
    let p = tmp("repo");
    std::fs::create_dir_all(&p).unwrap();
    git2::Repository::init(&p).unwrap();
    let s = GitStore::open(&p).unwrap();
    (p, s)
}
fn issue(s: &GitStore, title: &str) -> String {
    let ev = Event::new("issue.opened", None, me("a@x"), data(&[("title", json!(title)), ("body", json!("")), ("labels", strs(&[]))]));
    s.append(&ev).unwrap();
    ev.id
}
fn comment(s: &GitStore, id: &str, who: &str, txt: &str) {
    s.append(&Event::new("issue.commented", Some(id), me(who), data(&[("body", json!(txt))]))).unwrap();
}

#[test]
fn grava_e_le_na_ref_sem_tocar_a_branch_atual() {
    let (_p, s) = novo_repo();
    let id = issue(&s, "bug no login");
    comment(&s, &id, "b@x", "reproduzi aqui");
    s.append(&Event::new("issue.closed", Some(&id), me("a@x"), data(&[("reason", json!("done"))]))).unwrap();

    let it = replay(&s.load(&id).unwrap().events).unwrap();
    assert_eq!(it.title, "bug no login");
    assert_eq!(it.comments, 1);
    assert_eq!(it.state, State::Closed);
    assert_eq!(s.targets().unwrap(), vec![id.clone()]);

    // Só a ref meta existe: nada de branch/HEAD poluído.
    let repo = s.repo();
    assert!(repo.find_reference(REF).is_ok());
    assert!(repo.head().is_err(), "HEAD não deve ter commits");
    // 3 eventos + criação = 4 commits na ref.
    let mut w = repo.revwalk().unwrap();
    w.push_ref(REF).unwrap();
    assert_eq!(w.count(), 3);
}

#[test]
fn imutavel_e_valida_no_backend_git() {
    let (_p, s) = novo_repo();
    let ev = Event::new("issue.opened", None, me("a@x"), data(&[("title", json!("x"))]));
    s.append(&ev).unwrap();
    assert!(matches!(s.append(&ev), Err(StoreError::Exists)));

    let mut mau = Event::new("issue.opened", None, me("a@x"), data(&[("title", json!("x"))]));
    mau.target = "../../etc".into();
    assert!(matches!(s.append(&mau), Err(StoreError::Invalid(_))));
    assert!(s.load("../../etc").is_err());
}

#[test]
fn escritas_concorrentes_nao_perdem_eventos() {
    let (p, s) = novo_repo();
    let id = issue(&s, "corrida");
    let n_threads = 4;
    let por_thread = 15;
    let mut hs = vec![];
    for t in 0..n_threads {
        let (p, id) = (p.clone(), id.clone());
        hs.push(std::thread::spawn(move || {
            let s = GitStore::open(&p).unwrap(); // um handle por thread
            for i in 0..por_thread {
                comment(&s, &id, "t@x", &format!("t{t}-{i}"));
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
    }
    let it = replay(&s.load(&id).unwrap().events).unwrap();
    assert_eq!(it.comments, n_threads * por_thread);
}

#[test]
fn sync_offline_dois_devs_sem_conflito() {
    // "servidor" = repositório bare local (poderia ser pasta de rede, pendrive ou GitHub).
    let server = tmp("server");
    git2::Repository::init_bare(&server).unwrap();

    let (pa, a) = novo_repo();
    let (pb, b) = novo_repo();
    git(&pa, &["remote", "add", "origin", server.to_str().unwrap()]);
    git(&pb, &["remote", "add", "origin", server.to_str().unwrap()]);

    // A cria a issue e sincroniza; B ainda não sabe de nada.
    let id = issue(&a, "feature offline");
    assert!(sync(&a, "origin").unwrap().contains("enviado"));

    // B puxa, e então os dois trabalham SEM internet em paralelo.
    sync(&b, "origin").unwrap();
    comment(&a, &id, "a@x", "comentário do A");
    comment(&b, &id, "b@x", "comentário do B");
    b.append(&Event::new("issue.labeled", Some(&id), me("b@x"), data(&[("add", strs(&["urgente"])), ("remove", strs(&[]))]))).unwrap();

    // A volta online primeiro, depois B: histórias divergentes viram merge limpo.
    sync(&a, "origin").unwrap();
    let r = sync(&b, "origin").unwrap();
    assert!(r.contains("merge feito (0 conflito"), "{r}");
    sync(&a, "origin").unwrap();

    for s in [&a, &b] {
        let it = replay(&s.load(&id).unwrap().events).unwrap();
        assert_eq!(it.comments, 2, "os dois comentários precisam existir nos dois lados");
        assert!(it.labels.contains("urgente"));
    }
}

#[test]
fn merge_detecta_adulteracao_e_mantem_versao_local() {
    let (_p, a) = novo_repo();
    let id = issue(&a, "original");
    // Cria um segundo repo com o MESMO caminho mas conteúdo diferente.
    let (_q, b) = novo_repo();
    let ev = a.load(&id).unwrap().events.remove(0);
    let mut falso = ev.clone();
    falso.data = data(&[("title", json!("adulterado"))]);
    // grava o falso direto na ref de B com o mesmo id/target (bypass do append para simular ataque)
    let bytes = serde_json::to_vec_pretty(&falso).unwrap();
    let blob = b.repo().blob(&bytes).unwrap();
    let mut sub = b.repo().treebuilder(None).unwrap();
    sub.insert(format!("{}.json", ev.id), blob, 0o100644).unwrap();
    let mut root = b.repo().treebuilder(None).unwrap();
    root.insert(&ev.target, sub.write().unwrap(), 0o040000).unwrap();
    let tree = b.repo().find_tree(root.write().unwrap()).unwrap();
    let sig = git2::Signature::now("x", "x@x").unwrap();
    let c = b.repo().commit(None, &sig, &sig, "falso", &tree, &[]).unwrap();
    b.repo().reference("refs/meta/remote/b", c, true, "t").unwrap();
    // traz o commit de B para o repo A como ref remota
    let mut remote = a.repo().remote_anonymous(_q.to_str().unwrap()).unwrap();
    remote.fetch(&["+refs/meta/remote/b:refs/meta/remote/b"], None, None).unwrap();

    let out = a.merge_ref("refs/meta/remote/b").unwrap();
    assert_eq!(out, MergeOutcome::Merged { conflicts: 1 });
    let it = replay(&a.load(&id).unwrap().events).unwrap();
    assert_eq!(it.title, "original", "versão local prevalece");
}

#[test]
fn sync_recusa_remote_inexistente_ou_malicioso() {
    let (_p, s) = novo_repo();
    for nome in ["--upload-pack=touch /tmp/pwned", "ext::sh -c touch% /tmp/pwned", "naoexiste", "", "../x"] {
        assert!(sync(&s, nome).is_err(), "deveria recusar {nome:?}");
    }
    assert!(!std::path::Path::new("/tmp/pwned").exists());
}
