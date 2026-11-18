//! Estado atual = replay dos eventos.
use crate::event::{Event, Kind};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Issue,
    Pr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub title: String,
    pub body: String,
    pub state: State,
    pub author: String,
    pub labels: BTreeSet<String>,
    pub assignees: BTreeSet<String>,
    pub comments: usize,
    pub created_at: String,
    pub updated_at: String,
    // Campos de PR
    pub base: String,
    pub head: String,
    pub head_sha: String,
    /// Última revisão de cada autor: (estado, sha revisado).
    pub reviews: BTreeMap<String, (String, String)>,
    /// Último resultado de checks: (sha, tudo passou).
    pub checks: Option<(String, bool)>,
}

impl Item {
    /// Regra de merge: PR aberto, aprovado no head_sha ATUAL, sem pedido de mudança e checks verdes.
    pub fn merge_blockers(&self) -> Vec<String> {
        let mut b = Vec::new();
        if self.kind != ItemKind::Pr {
            b.push("não é um PR".into());
            return b;
        }
        if self.state != State::Open {
            b.push("PR não está aberto".into());
        }
        let on_head: Vec<&(String, String)> =
            self.reviews.values().filter(|(_, sha)| *sha == self.head_sha).collect();
        if on_head.iter().any(|(st, _)| st == "changes_requested") {
            b.push("há mudanças solicitadas no commit atual".into());
        }
        if !on_head.iter().any(|(st, _)| st == "approved") {
            b.push("falta aprovação no commit atual (aprovações antigas ficam desatualizadas)".into());
        }
        match &self.checks {
            Some((sha, true)) if *sha == self.head_sha => {}
            Some((sha, _)) if *sha != self.head_sha => b.push("checks desatualizados para o commit atual".into()),
            Some(_) => b.push("checks falhando".into()),
            None => b.push("checks ainda não rodaram".into()),
        }
        b
    }
}

/// Replay. Eventos devem vir ordenados (a `Store` já entrega ordenado).
pub fn replay(events: &[Event]) -> Option<Item> {
    let mut it: Option<Item> = None;
    for ev in events {
        match ev.parsed() {
            Kind::IssueOpened { title, body, labels } => {
                it = Some(blank(ev, ItemKind::Issue, title, body));
                if let Some(i) = it.as_mut() {
                    i.labels = labels.into_iter().collect();
                }
            }
            Kind::PrOpened { title, body, base, head, head_sha } => {
                let mut i = blank(ev, ItemKind::Pr, title, body);
                i.base = base;
                i.head = head;
                i.head_sha = head_sha;
                it = Some(i);
            }
            other => {
                let Some(i) = it.as_mut() else { continue };
                apply(i, ev, other);
                i.updated_at = ev.ts.clone();
            }
        }
    }
    it
}

fn blank(ev: &Event, kind: ItemKind, title: String, body: String) -> Item {
    Item {
        id: ev.target.clone(),
        kind,
        title,
        body,
        state: State::Open,
        author: ev.author.name.clone(),
        labels: BTreeSet::new(),
        assignees: BTreeSet::new(),
        comments: 0,
        created_at: ev.ts.clone(),
        updated_at: ev.ts.clone(),
        base: String::new(),
        head: String::new(),
        head_sha: String::new(),
        reviews: BTreeMap::new(),
        checks: None,
    }
}

fn apply(i: &mut Item, ev: &Event, k: Kind) {
    match k {
        Kind::IssueEdited { title, body } => {
            if let Some(t) = title {
                i.title = t;
            }
            if let Some(b) = body {
                i.body = b;
            }
        }
        Kind::IssueCommented { .. } | Kind::PrCommented { .. } => i.comments += 1,
        Kind::IssueLabeled { add, remove } => {
            i.labels.extend(add);
            for r in remove {
                i.labels.remove(&r);
            }
        }
        Kind::IssueAssigned { add, remove } => {
            i.assignees.extend(add);
            for r in remove {
                i.assignees.remove(&r);
            }
        }
        Kind::IssueClosed { .. } | Kind::PrClosed => {
            if i.state != State::Merged {
                i.state = State::Closed;
            }
        }
        Kind::IssueReopened | Kind::PrReopened => {
            if i.state == State::Closed {
                i.state = State::Open;
            }
        }
        Kind::PrUpdated { head_sha } => i.head_sha = head_sha,
        Kind::PrReviewed { state, sha, .. } => {
            i.reviews.insert(ev.author.email.clone(), (state, sha));
        }
        Kind::PrChecks { sha, all_pass } => i.checks = Some((sha, all_pass)),
        Kind::PrMerged { .. } => {
            // Merge só vale se as regras passam no momento do evento.
            if i.merge_blockers().is_empty() {
                i.state = State::Merged;
            }
        }
        _ => {}
    }
}
