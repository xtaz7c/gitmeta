# gitmeta

Issues e PRs locais para Git. Funciona offline; sincroniza quando houver remoto (GitHub, servidor local, pasta, pendrive).

## Como funciona
- Cada ação (abrir issue, comentar, aprovar PR...) é um **evento JSON imutável** (`<id>.json`, id = ULID).
- Eventos ficam na ref **`refs/meta/issues`** do próprio repositório, fora das suas branches (HEAD e `main` não são tocados).
- O estado atual é calculado por **replay** dos eventos.
- Sync = união de árvores: dois devs offline sempre mesclam **sem conflito**.

## Uso
```
cargo build --release
cd meu-repo-git
gitmeta issue-open "Título" "corpo"      # imprime o id
gitmeta list
gitmeta show <id>
gitmeta comment <id> "texto"
gitmeta pr-open "Título" main feat/x <sha>
gitmeta review <id> approved <sha>
gitmeta checks <id> <sha> pass
gitmeta merge <id> <sha-do-merge>        # bloqueia se faltar aprovação/checks no SHA atual
git remote add origin <url-ou-pasta>
gitmeta sync origin                      # fetch + merge + push da ref meta
```
Autor via `GITMETA_NAME` e `GITMETA_EMAIL`. Para usar só uma pasta (sem Git): `--dir PASTA`.

## Segurança
- Sem `unsafe` (`#![forbid(unsafe_code)]`).
- IDs validados como ULID (sem path traversal); limites de tamanho; tipos desconhecidos rejeitados.
- Eventos nunca são sobrescritos; troca da ref é atômica (compare-and-swap com retry).
- `sync` só aceita remotes já configurados, sem shell, com `protocol.ext.allow=never`.
- Merge só vale com aprovação no SHA atual e checks verdes (o replay ignora merges inválidos).
- Pasta: permissões 0700/0600 em Unix.

## Plataformas
Código portável (Linux, macOS, Windows). Testado localmente só no Linux; o CI (`.github/workflows/ci.yml`) roda os testes nos três.
Requer o `git` instalado apenas para `sync` (rede).

## Pendências
- Motor de workflow que gera `pr.checks` sozinho.
- Verificação de assinatura (`sig`).
- Cache SQLite para a GUI.

## Downloads
Cada tag `vX.Y.Z` gera uma release no GitHub com: AppImage (Linux), `.tar.gz` (Linux e macOS arm64/x86_64), `.zip` (Windows) e `SHA256SUMS`.
```
chmod +x gitmeta-*-x86_64.AppImage
./gitmeta-*-x86_64.AppImage --version
sha256sum -c SHA256SUMS --ignore-missing   # confere a integridade
```
