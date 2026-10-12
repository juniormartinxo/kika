# kika

CLI em Rust para localizar e remover lixo comum de projetos frontend e diretórios de trabalho.

Escopo inicial:

- `node_modules`
- `dist`
- `.next`
- `target` de projetos Rust (limpeza via `cargo clean --offline`, apenas quando há `Cargo.toml` irmão e `cargo` no `PATH`)
- arquivos temporários como `.tmp`, `.temp`, `.bak`, `.old`, `.orig`, `.rej`, `.swp` e `.swo`
- arquivos `Zone.Identifier` (alternate data streams criados pelo Windows/WSL)
- arquivos antigos, mas apenas em modo explícito com cutoff em dias

Caches globais de ferramentas (fora do diretório escaneado, apenas com flag explícita):

| Flag               | Comando executado no apply      |
| ------------------ | ------------------------------- |
| `--docker-builder` | `docker builder prune --force`  |
| `--pnpm-store`     | `pnpm store prune`              |
| `--uv-cache`       | `uv cache clean`                |
| `--yarn-cache`     | `yarn cache clean`              |
| `--npm-cache`      | `npm cache clean --force`       |
| `--all-caches`     | todos os anteriores             |

## Uso

`cargo run -- [path]`

Executa um `dry-run` e mostra o que seria removido.

`cargo run -- clean [path] --apply`

Pede confirmação e, se você confirmar, remove de fato os diretórios e arquivos encontrados.

`cargo run -- clean [path] --apply --yes`

Remove sem prompt de confirmação. Esse modo é o mais adequado para automação.

`cargo run -- report [path] --output cleanup-report.txt`

Gera um relatório e salva em arquivo, sem apagar nada.

`cargo run -- report [path] --include-old-files --older-than-days 90`

Gera um relatório incluindo arquivos regulares mais antigos que 90 dias.

`cargo run -- clean [path] --apply --include-old-files --older-than-days 90`

Além das regras padrão, remove arquivos regulares mais antigos que 90 dias.

`cargo run -- report [path] --all-caches`

Mostra quanto ocupam os caches do Docker, pnpm, uv, yarn e npm, sem limpar nada.

`cargo run -- clean [path] --apply --docker-builder --pnpm-store`

Além das regras padrão, limpa o build cache do Docker e o store do pnpm.

### Caches globais

- O tamanho no relatório é uma estimativa: para Docker vem do `Reclaimable` de `docker system df`; para os demais é o tamanho do diretório de cache (no pnpm é um teto, já que `pnpm store prune` mantém pacotes ainda referenciados)
- No apply, o espaço informado é o realmente liberado (medido de novo após a limpeza ou lido da saída do Docker)
- Ferramenta ausente ou com erro gera um aviso e o cache é ignorado, sem interromper o restante
- `npm`, `pnpm` e `yarn` são procurados primeiro no `PATH`; se não estiverem lá (cron, shell não interativo), o kika usa o Node do fnm (`$FNM_DIR`, `~/.local/share/fnm`, `~/.fnm`) ou do nvm (`$NVM_DIR`, `~/.nvm`), preferindo o alias `default` e depois a versão mais recente instalada. O `bin` dessa versão é colocado no `PATH` do processo filho para que o `node` seja encontrado

## Segurança

- O modo padrão é `dry-run`
- Remoção real só acontece com `clean --apply`
- `clean --apply` pede confirmação, a menos que você passe `--yes`
- Limpeza de arquivos antigos exige `--include-old-files` e `--older-than-days`
- O scanner ignora links simbólicos e `.git`
- Diretórios `target` só são tratados em projetos Rust (com `Cargo.toml` irmão); demais `target/` são ignorados
- Sem `cargo` no `PATH`, o kika reporta o `target/` mas falha o apply com aviso, sem remoção forçada
