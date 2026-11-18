//! Sync offline-first com remotes já configurados (GitHub, servidor local, pasta, pendrive).
//! Usa o `git` instalado só para a rede; nunca passa pelo shell e só aceita nomes de remote existentes.
use crate::gitstore::{GitStore, MergeOutcome, REF};
use crate::store::StoreError;
use std::process::Command;

fn nome_valido(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && !n.starts_with(['-', '.'])
        && n.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}

/// Executa o git e devolve (código de saída, saída). Nunca depende do idioma da mensagem.
fn git(workdir: &std::path::Path, args: &[&str]) -> Result<(Option<i32>, String), StoreError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workdir)
        // Endurecimento: bloqueia transportes que executam comandos arbitrários.
        .args(["-c", "protocol.ext.allow=never"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| StoreError::Git(format!("não consegui executar o git: {e}")))?;
    let txt = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Ok((out.status.code(), txt))
}

/// fetch -> merge -> push. Retorna um resumo legível.
pub fn sync(store: &GitStore, remote: &str) -> Result<String, StoreError> {
    if !nome_valido(remote) {
        return Err(StoreError::Invalid("nome de remote inválido".into()));
    }
    // Só remotes já configurados: nada de URLs vindas de argumento.
    if store.repo().find_remote(remote).is_err() {
        return Err(StoreError::Invalid(format!("remote '{remote}' não existe (use `git remote add`)")));
    }
    let workdir = store.repo().workdir().unwrap_or_else(|| store.repo().path());
    let tracking = format!("refs/meta/remote/{remote}");
    let refspec = format!("+{REF}:{tracking}");

    let mut resumo = String::new();
    let (code, txt) = git(workdir, &["fetch", "--no-tags", "--", remote, &refspec])?;
    if code == Some(0) {
        resumo.push_str(&match store.merge_ref(&tracking)? {
            MergeOutcome::UpToDate => "local já estava à frente ou igual".to_string(),
            MergeOutcome::FastForward => "trouxe novidades do remoto".to_string(),
            MergeOutcome::Merged { conflicts } => format!("merge feito ({conflicts} conflito(s) ignorado(s))"),
        });
    } else {
        // Desempate por código de saída (2 = ref não existe no remoto), igual em qualquer idioma.
        let (c, _) = git(workdir, &["ls-remote", "--exit-code", "--", remote, REF])?;
        if c == Some(2) {
            resumo.push_str("remoto ainda sem dados");
        } else {
            return Err(StoreError::Git(format!(
                "fetch falhou (ls-remote saiu com {c:?}; esperado 2 se o remoto estiver vazio): {}",
                txt.trim()
            )));
        }
    }

    let push = format!("{REF}:{REF}");
    let (code, txt) = git(workdir, &["push", "--", remote, &push])?;
    if code != Some(0) {
        return Err(StoreError::Git(format!("push falhou: {}", txt.trim())));
    }
    resumo.push_str("; enviado ao remoto");
    Ok(resumo)
}
