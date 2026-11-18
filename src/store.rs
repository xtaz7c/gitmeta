//! Armazenamento em pasta: `<raiz>/<target>/<id>.json`.
//! Escrita atômica (tmp + hard_link), nunca sobrescreve, fsync, permissões 0700/0600 (Unix).
use crate::event::{is_ulid, Event, MAX_EVENT_BYTES};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Git(String),
    Invalid(String),
    Exists,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "erro de E/S: {e}"),
            StoreError::Git(m) => write!(f, "erro de git: {m}"),
            StoreError::Invalid(m) => write!(f, "inválido: {m}"),
            StoreError::Exists => write!(f, "evento já existe (eventos são imutáveis)"),
        }
    }
}
impl std::error::Error for StoreError {}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}
impl From<git2::Error> for StoreError {
    fn from(e: git2::Error) -> Self {
        StoreError::Git(e.message().to_owned())
    }
}

pub struct Loaded {
    pub events: Vec<Event>,
    /// Itens ignorados (corrompidos, grandes demais, nome inválido).
    pub skipped: Vec<String>,
}

/// Contrato comum aos backends (pasta e ref do Git).
pub trait EventStore {
    fn append(&self, ev: &Event) -> Result<(), StoreError>;
    fn load(&self, target: &str) -> Result<Loaded, StoreError>;
    fn targets(&self) -> Result<Vec<String>, StoreError>;
}

/// Valida o conteúdo lido: precisa bater com o nome do arquivo e com a pasta.
pub(crate) fn parse_stored(bytes: &[u8], stem: &str, target: &str) -> Option<Event> {
    if bytes.len() > MAX_EVENT_BYTES {
        return None;
    }
    let ev: Event = serde_json::from_slice(bytes).ok()?;
    (ev.id == stem && ev.target == target && ev.validate().is_ok()).then_some(ev)
}

/// Ordem: ts, com o ULID como desempate. O evento de criação vem sempre primeiro.
pub(crate) fn finalize(events: &mut [Event], target: &str) {
    events.sort_by(|a, b| (&a.ts, &a.id).cmp(&(&b.ts, &b.id)));
    if let Some(p) = events.iter().position(|e| e.id == target) {
        events[..=p].rotate_right(1);
    }
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: impl AsRef<Path>) -> Result<Store, StoreError> {
        let root = root.as_ref().to_path_buf();
        make_dir(&root)?;
        Ok(Store { root })
    }

    pub fn append(&self, ev: &Event) -> Result<(), StoreError> {
        ev.validate().map_err(StoreError::Invalid)?;
        let bytes = serde_json::to_vec_pretty(ev).map_err(|e| StoreError::Invalid(e.to_string()))?;
        if bytes.len() > MAX_EVENT_BYTES {
            return Err(StoreError::Invalid("evento passa do limite de tamanho".into()));
        }
        let dir = self.root.join(&ev.target); // target já validado como ULID
        make_dir(&dir)?;
        let tmp = dir.join(format!(".tmp-{}", ev.id));
        let fin = dir.join(format!("{}.json", ev.id));

        let mut f = new_file(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        drop(f);
        // hard_link falha se o destino existir: garante imutabilidade.
        let res = fs::hard_link(&tmp, &fin);
        let _ = fs::remove_file(&tmp);
        match res {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(StoreError::Exists),
            Err(_) => {
                // Sistemas sem hard link (FAT/exFAT, alguns compartilhamentos): create_new também não sobrescreve.
                let mut f = match new_file(&fin) {
                    Ok(f) => f,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(StoreError::Exists),
                    Err(e) => return Err(e.into()),
                };
                f.write_all(&bytes)?;
                f.sync_all()?;
            }
        }
        sync_dir(&dir);
        Ok(())
    }

    pub fn load(&self, target: &str) -> Result<Loaded, StoreError> {
        if !is_ulid(target) {
            return Err(StoreError::Invalid("target deve ser ULID válido".into()));
        }
        let dir = self.root.join(target);
        let mut events = Vec::new();
        let mut skipped = Vec::new();
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Loaded { events, skipped }),
            Err(e) => return Err(e.into()),
        };
        for entry in rd {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".json") else { continue };
            if !is_ulid(stem) {
                skipped.push(name);
                continue;
            }
            let meta = entry.metadata()?;
            if !meta.is_file() || meta.len() as usize > MAX_EVENT_BYTES {
                skipped.push(name);
                continue;
            }
            match fs::read(entry.path()).ok().and_then(|b| parse_stored(&b, stem, target)) {
                Some(ev) => events.push(ev),
                None => skipped.push(name),
            }
        }
        finalize(&mut events, target);
        Ok(Loaded { events, skipped })
    }

    pub fn targets(&self) -> Result<Vec<String>, StoreError> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if is_ulid(&name) {
                out.push(name);
            }
        }
        out.sort();
        Ok(out)
    }
}

impl EventStore for Store {
    fn append(&self, ev: &Event) -> Result<(), StoreError> {
        Store::append(self, ev)
    }
    fn load(&self, target: &str) -> Result<Loaded, StoreError> {
        Store::load(self, target)
    }
    fn targets(&self) -> Result<Vec<String>, StoreError> {
        Store::targets(self)
    }
}

/// fsync da pasta: necessário em Unix para durabilidade; no Windows não existe e é ignorado.
#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
}
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

#[cfg(unix)]
fn make_dir(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(p)
}
#[cfg(not(unix))]
fn make_dir(p: &Path) -> std::io::Result<()> {
    // Windows: a pasta herda a ACL do perfil do usuário (por padrão só o próprio usuário e admins).
    fs::create_dir_all(p)
}

#[cfg(unix)]
fn new_file(p: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new().write(true).create_new(true).mode(0o600).open(p)
}
#[cfg(not(unix))]
fn new_file(p: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(p)
}
