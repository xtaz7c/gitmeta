use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const SCHEMA_V: u32 = 1;
pub const MAX_EVENT_BYTES: usize = 64 * 1024;
pub const MAX_TITLE: usize = 256;
pub const MAX_BODY: usize = 32 * 1024;
pub const MAX_LIST: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub v: u32,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub target: String,
    pub author: Author,
    pub ts: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub sig: Option<String>,
}

/// Evento tipado. `Unknown` existe para compatibilidade: tipos novos são ignorados, nunca quebram.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    IssueOpened { title: String, body: String, labels: Vec<String> },
    IssueEdited { title: Option<String>, body: Option<String> },
    IssueCommented { body: String },
    IssueLabeled { add: Vec<String>, remove: Vec<String> },
    IssueAssigned { add: Vec<String>, remove: Vec<String> },
    IssueClosed { reason: String },
    IssueReopened,
    PrOpened { title: String, body: String, base: String, head: String, head_sha: String },
    PrUpdated { head_sha: String },
    PrCommented { body: String },
    PrReviewed { state: String, body: String, sha: String },
    PrChecks { sha: String, all_pass: bool },
    PrMerged { merge_sha: String, strategy: String },
    PrClosed,
    PrReopened,
    Unknown,
}

fn s(d: &Value, k: &str) -> Option<String> {
    d.get(k)?.as_str().map(str::to_owned)
}
fn list(d: &Value, k: &str) -> Vec<String> {
    d.get(k)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

impl Event {
    /// Cria um evento novo (id e ts gerados aqui).
    pub fn new(kind: &str, target: Option<&str>, author: Author, data: Value) -> Event {
        let id = next_ulid().to_string();
        Event {
            v: SCHEMA_V,
            target: target.map(str::to_owned).unwrap_or_else(|| id.clone()),
            id,
            kind: kind.to_owned(),
            author,
            ts: crate::time::now_utc(),
            data,
            sig: None,
        }
    }

    pub fn parsed(&self) -> Kind {
        let d = &self.data;
        match self.kind.as_str() {
            "issue.opened" => Kind::IssueOpened {
                title: s(d, "title").unwrap_or_default(),
                body: s(d, "body").unwrap_or_default(),
                labels: list(d, "labels"),
            },
            "issue.edited" => Kind::IssueEdited { title: s(d, "title"), body: s(d, "body") },
            "issue.commented" => Kind::IssueCommented { body: s(d, "body").unwrap_or_default() },
            "issue.labeled" => Kind::IssueLabeled { add: list(d, "add"), remove: list(d, "remove") },
            "issue.assigned" => Kind::IssueAssigned { add: list(d, "add"), remove: list(d, "remove") },
            "issue.closed" => Kind::IssueClosed { reason: s(d, "reason").unwrap_or_else(|| "done".into()) },
            "issue.reopened" => Kind::IssueReopened,
            "pr.opened" => Kind::PrOpened {
                title: s(d, "title").unwrap_or_default(),
                body: s(d, "body").unwrap_or_default(),
                base: s(d, "base").unwrap_or_default(),
                head: s(d, "head").unwrap_or_default(),
                head_sha: s(d, "head_sha").unwrap_or_default(),
            },
            "pr.updated" => Kind::PrUpdated { head_sha: s(d, "head_sha").unwrap_or_default() },
            "pr.commented" => Kind::PrCommented { body: s(d, "body").unwrap_or_default() },
            "pr.reviewed" => Kind::PrReviewed {
                state: s(d, "state").unwrap_or_default(),
                body: s(d, "body").unwrap_or_default(),
                sha: s(d, "sha").unwrap_or_default(),
            },
            "pr.checks" => Kind::PrChecks {
                sha: s(d, "sha").unwrap_or_default(),
                all_pass: d
                    .get("results")
                    .and_then(Value::as_array)
                    .map(|r| !r.is_empty() && r.iter().all(|x| x.get("status").and_then(Value::as_str) == Some("pass")))
                    .unwrap_or(false),
            },
            "pr.merged" => Kind::PrMerged {
                merge_sha: s(d, "merge_sha").unwrap_or_default(),
                strategy: s(d, "strategy").unwrap_or_else(|| "merge".into()),
            },
            "pr.closed" => Kind::PrClosed,
            "pr.reopened" => Kind::PrReopened,
            _ => Kind::Unknown,
        }
    }

    /// Validação rígida antes de gravar.
    pub fn validate(&self) -> Result<(), String> {
        if self.v != SCHEMA_V {
            return Err(format!("versão de schema não suportada: {}", self.v));
        }
        if !is_ulid(&self.id) || !is_ulid(&self.target) {
            return Err("id/target devem ser ULID válido".into());
        }
        if !crate::time::is_valid_ts(&self.ts) {
            return Err("ts fora do formato YYYY-MM-DDTHH:MM:SSZ".into());
        }
        if self.author.name.is_empty() || self.author.name.len() > 128 || self.author.email.len() > 256 {
            return Err("autor inválido".into());
        }
        if matches!(self.parsed(), Kind::Unknown) {
            return Err(format!("tipo de evento desconhecido: {}", self.kind));
        }
        if let Some(t) = s(&self.data, "title") {
            if t.is_empty() || t.chars().count() > MAX_TITLE {
                return Err("título vazio ou grande demais".into());
            }
        }
        if let Some(b) = s(&self.data, "body") {
            if b.len() > MAX_BODY {
                return Err("corpo grande demais".into());
            }
        }
        for k in ["labels", "add", "remove"] {
            if list(&self.data, k).len() > MAX_LIST {
                return Err(format!("lista '{k}' grande demais"));
            }
        }
        if self.kind.ends_with(".opened") && self.id != self.target {
            return Err("evento de criação deve ter target == id".into());
        }
        Ok(())
    }
}

/// ULID: 26 caracteres Crockford base32, primeiro <= '7' (128 bits).
pub fn is_ulid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 26
        && b[0] <= b'7'
        && b.iter().all(|c| matches!(c, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z'))
}

pub fn data(pairs: &[(&str, Value)]) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in pairs {
        m.insert((*k).to_owned(), v.clone());
    }
    Value::Object(m)
}

pub fn strs(items: &[&str]) -> Value {
    json!(items)
}

/// ULID monotônico dentro do processo: eventos criados no mesmo milissegundo mantêm a ordem.
fn next_ulid() -> ulid::Ulid {
    use std::sync::{Mutex, OnceLock};
    static GEN: OnceLock<Mutex<ulid::Generator>> = OnceLock::new();
    let g = GEN.get_or_init(|| Mutex::new(ulid::Generator::new()));
    match g.lock() {
        Ok(mut g) => g.generate().unwrap_or_else(|_| ulid::Ulid::new()),
        Err(_) => ulid::Ulid::new(),
    }
}
