//! Backend na ref `refs/meta/issues` do próprio repositório Git.
//! Cada append = 1 commit novo (árvore `<target>/<id>.json`), com troca atômica da ref (compare-and-swap).
//! Sync offline = união de árvores, sem conflito (nomes de arquivo são ULIDs únicos).
use crate::event::{is_ulid, Event, MAX_EVENT_BYTES};
use crate::store::{finalize, parse_stored, EventStore, Loaded, StoreError};
use git2::{Commit, ErrorCode, FileMode, ObjectType, Oid, Repository, Signature, Tree};
use std::path::Path;

pub const REF: &str = "refs/meta/issues";
const TENTATIVAS: u32 = 64;

/// Erros transitórios de disputa pela ref. No Windows o libgit2 reporta "acesso negado" /
/// violação de compartilhamento (classe Os) em vez de "locked" quando duas escritas colidem.
fn retryable(e: &git2::Error) -> bool {
    matches!(e.code(), ErrorCode::Exists | ErrorCode::Modified | ErrorCode::Locked)
        || matches!(
            e.class(),
            git2::ErrorClass::Os | git2::ErrorClass::Reference | git2::ErrorClass::Odb | git2::ErrorClass::Filesystem
        )
}

/// Espera curta com jitter entre tentativas de trocar a ref (evita briga em lockstep).
fn backoff(tentativa: u32) {
    let base = 50u64 << tentativa.min(7); // 50µs .. ~6,4ms
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()) % base.max(1))
        .unwrap_or(0);
    std::thread::sleep(std::time::Duration::from_micros(base + jitter));
}

pub struct GitStore {
    repo: Repository,
    /// Último erro transitório visto (aparece na mensagem se as tentativas acabarem).
    ultimo: std::cell::RefCell<String>,
}

enum Passo {
    Feito,
    Existe,
    Repetir,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    UpToDate,
    FastForward,
    /// `conflicts`: mesmo caminho com conteúdo diferente (adulteração). Mantém a versão local.
    Merged { conflicts: usize },
}

impl GitStore {
    pub fn open(path: impl AsRef<Path>) -> Result<GitStore, StoreError> {
        Ok(GitStore { repo: Repository::open(path)?, ultimo: Default::default() })
    }

    pub fn discover(path: impl AsRef<Path>) -> Result<GitStore, StoreError> {
        Ok(GitStore { repo: Repository::discover(path)?, ultimo: Default::default() })
    }

    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    fn tip_raw(&self, name: &str) -> Result<Option<Commit<'_>>, git2::Error> {
        match self.repo.find_reference(name) {
            Ok(r) => Ok(Some(r.peel_to_commit()?)),
            Err(e) if e.code() == ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn tip_of(&self, name: &str) -> Result<Option<Commit<'_>>, StoreError> {
        Ok(self.tip_raw(name)?)
    }

    fn note(&self, e: &git2::Error) {
        *self.ultimo.borrow_mut() = e.message().to_owned();
    }

    fn signature(ev: &Event) -> Signature<'static> {
        Signature::now(&ev.author.name, &ev.author.email)
            .or_else(|_| Signature::now("gitmeta", "gitmeta@local"))
            .expect("assinatura padrão é sempre válida")
    }

    /// Troca atômica da ref: só vale se ninguém mexeu desde que lemos `old`.
    fn swap_ref(&self, old: Option<Oid>, new: Oid, msg: &str) -> Result<bool, StoreError> {
        let res = match old {
            None => self.repo.reference(REF, new, false, msg),
            Some(o) => self.repo.reference_matching(REF, new, true, o, msg),
        };
        match res {
            Ok(_) => Ok(true),
            Err(e) if retryable(&e) => {
                self.note(&e);
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Mescla outra ref (ex.: `refs/meta/remote/<nome>`) na local.
    pub fn merge_ref(&self, other_ref: &str) -> Result<MergeOutcome, StoreError> {
        for tentativa in 0..TENTATIVAS {
            if tentativa > 0 {
                backoff(tentativa);
            }
            let Some(other) = self.tip_of(other_ref)? else { return Ok(MergeOutcome::UpToDate) };
            let Some(local) = self.tip_of(REF)? else {
                if self.swap_ref(None, other.id(), "gitmeta: adotar remoto")? {
                    return Ok(MergeOutcome::FastForward);
                }
                continue;
            };
            if local.id() == other.id() || self.repo.graph_descendant_of(local.id(), other.id())? {
                return Ok(MergeOutcome::UpToDate);
            }
            if self.repo.graph_descendant_of(other.id(), local.id())? {
                if self.swap_ref(Some(local.id()), other.id(), "gitmeta: fast-forward")? {
                    return Ok(MergeOutcome::FastForward);
                }
                continue;
            }
            let (tree_id, conflicts) = self.union_roots(&local.tree()?, &other.tree()?)?;
            let tree = self.repo.find_tree(tree_id)?;
            let sig = Signature::now("gitmeta", "gitmeta@local")?;
            let oid = self.repo.commit(None, &sig, &sig, "gitmeta: merge", &tree, &[&local, &other])?;
            if self.swap_ref(Some(local.id()), oid, "gitmeta: merge")? {
                return Ok(MergeOutcome::Merged { conflicts });
            }
        }
        Err(StoreError::Git("ref mudou durante o merge; tente de novo".into()))
    }

    fn union_roots(&self, a: &Tree, b: &Tree) -> Result<(Oid, usize), StoreError> {
        let mut conflicts = 0;
        let mut root = self.repo.treebuilder(Some(a))?;
        for eb in b.iter() {
            let Some(name) = eb.name() else { continue };
            if !is_ulid(name) || eb.kind() != Some(ObjectType::Tree) {
                continue; // ignora qualquer coisa fora do formato
            }
            match a.get_name(name) {
                None => {
                    root.insert(name, eb.id(), FileMode::Tree.into())?;
                }
                Some(ea) => {
                    let sa = self.repo.find_tree(ea.id())?;
                    let sb = self.repo.find_tree(eb.id())?;
                    let mut sub = self.repo.treebuilder(Some(&sa))?;
                    for f in sb.iter() {
                        let Some(fname) = f.name() else { continue };
                        if f.kind() != Some(ObjectType::Blob) {
                            continue;
                        }
                        match sa.get_name(fname) {
                            None => {
                                sub.insert(fname, f.id(), FileMode::Blob.into())?;
                            }
                            Some(fa) if fa.id() != f.id() => conflicts += 1,
                            Some(_) => {}
                        }
                    }
                    root.insert(name, sub.write()?, FileMode::Tree.into())?;
                }
            }
        }
        Ok((root.write()?, conflicts))
    }
}

impl GitStore {
    /// Uma tentativa de gravar: lê a ponta, monta a árvore, comita e troca a ref de forma atômica.
    fn tentar(&self, ev: &Event, fname: &str, blob: Oid, sig: &Signature) -> Result<Passo, git2::Error> {
        let tip = self.tip_raw(REF)?;
        let base = tip.as_ref().map(Commit::tree).transpose()?;

        let sub_old = match base.as_ref().and_then(|t| t.get_name(&ev.target)) {
            Some(e) => Some(self.repo.find_tree(e.id())?),
            None => None,
        };
        let mut sub = self.repo.treebuilder(sub_old.as_ref())?;
        if sub.get(fname)?.is_some() {
            return Ok(Passo::Existe);
        }
        sub.insert(fname, blob, FileMode::Blob.into())?;
        let sub_id = sub.write()?;

        let mut root = self.repo.treebuilder(base.as_ref())?;
        root.insert(&ev.target, sub_id, FileMode::Tree.into())?;
        let tree = self.repo.find_tree(root.write()?)?;

        let msg = format!("{} {}", ev.kind, ev.target);
        let parents: Vec<&Commit> = tip.iter().collect();
        let oid = self.repo.commit(None, sig, sig, &msg, &tree, &parents)?;
        let res = match tip.as_ref().map(Commit::id) {
            None => self.repo.reference(REF, oid, false, &msg),
            Some(old) => self.repo.reference_matching(REF, oid, true, old, &msg),
        };
        match res {
            Ok(_) => Ok(Passo::Feito),
            // Outra escrita venceu: o chamador relê a ponta e repete.
            Err(e) if matches!(e.code(), ErrorCode::Exists | ErrorCode::Modified | ErrorCode::Locked) => Ok(Passo::Repetir),
            Err(e) => Err(e),
        }
    }
}

impl EventStore for GitStore {
    fn append(&self, ev: &Event) -> Result<(), StoreError> {
        ev.validate().map_err(StoreError::Invalid)?;
        let bytes = serde_json::to_vec_pretty(ev).map_err(|e| StoreError::Invalid(e.to_string()))?;
        if bytes.len() > MAX_EVENT_BYTES {
            return Err(StoreError::Invalid("evento passa do limite de tamanho".into()));
        }
        let fname = format!("{}.json", ev.id);
        let sig = Self::signature(ev);
        let blob = self.repo.blob(&bytes)?;

        for tentativa in 0..TENTATIVAS {
            if tentativa > 0 {
                backoff(tentativa);
            }
            match self.tentar(ev, &fname, blob, &sig) {
                Ok(Passo::Feito) => return Ok(()),
                Ok(Passo::Existe) => return Err(StoreError::Exists),
                Ok(Passo::Repetir) => {}
                Err(e) if retryable(&e) => self.note(&e),
                Err(e) => return Err(e.into()),
            }
        }
        Err(StoreError::Git(format!(
            "muita concorrência na ref ({TENTATIVAS} tentativas); último erro: {}",
            self.ultimo.borrow()
        )))
    }

    fn load(&self, target: &str) -> Result<Loaded, StoreError> {
        if !is_ulid(target) {
            return Err(StoreError::Invalid("target deve ser ULID válido".into()));
        }
        let mut events = Vec::new();
        let mut skipped = Vec::new();
        let Some(tip) = self.tip_of(REF)? else { return Ok(Loaded { events, skipped }) };
        let root = tip.tree()?;
        let Some(entry) = root.get_name(target) else { return Ok(Loaded { events, skipped }) };
        let sub = self.repo.find_tree(entry.id())?;
        for f in sub.iter() {
            let name = f.name().unwrap_or("?").to_owned();
            let Some(stem) = name.strip_suffix(".json") else { continue };
            if !is_ulid(stem) || f.kind() != Some(ObjectType::Blob) {
                skipped.push(name);
                continue;
            }
            let blob = self.repo.find_blob(f.id())?;
            match parse_stored(blob.content(), stem, target) {
                Some(ev) => events.push(ev),
                None => skipped.push(name),
            }
        }
        finalize(&mut events, target);
        Ok(Loaded { events, skipped })
    }

    fn targets(&self) -> Result<Vec<String>, StoreError> {
        let Some(tip) = self.tip_of(REF)? else { return Ok(Vec::new()) };
        let mut out: Vec<String> = tip
            .tree()?
            .iter()
            .filter(|e| e.kind() == Some(ObjectType::Tree))
            .filter_map(|e| e.name().map(str::to_owned))
            .filter(|n| is_ulid(n))
            .collect();
        out.sort();
        Ok(out)
    }
}
